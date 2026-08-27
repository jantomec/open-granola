//! Platform system-audio loopback — the "no bot joins your call" trick.
//! Each backend feeds the same 16 kHz mono ring buffer as the mic.

use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::SharedProducer;

#[cfg(target_os = "macos")]
pub use macos::start;

/// macOS: a Core Audio process tap (macOS 14.4+). A global tap captures the
/// mixed-down audio of every process — Zoom, Meet, Teams, a browser tab —
/// without a virtual driver and without a bot. The tap is wrapped in a
/// private aggregate device whose IOProc hands us the frames.
///
/// Requires the "System Audio Recording" permission (TCC prompts on first
/// use; `NSAudioCaptureUsageDescription` in Info.plist supplies the text).
#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use crate::audio::WHISPER_RATE;

    use core_foundation::array::CFArray;
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    use coreaudio::sys as ca;
    use objc2::rc::Id;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2::{class, msg_send, msg_send_id};
    use ringbuf::traits::Producer;
    use std::os::raw::c_void;

    use anyhow::{bail, Context};

    // These two live behind `#if defined(__OBJC__)` in AudioHardwareTapping.h
    // (they take a CATapDescription), so bindgen-built coreaudio-sys never
    // sees them and we declare them ourselves.
    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioHardwareCreateProcessTap(
            desc: *mut AnyObject,
            out_tap: *mut ca::AudioObjectID,
        ) -> ca::OSStatus;
        fn AudioHardwareDestroyProcessTap(tap: ca::AudioObjectID) -> ca::OSStatus;
    }

    /// Same fixed frame size the mic chain feeds its resampler.
    const CHUNK: usize = 1024;

    /// State owned by the IOProc. Lives in a Box inside `SystemTap`, which
    /// outlives the proc: Drop destroys the proc before the Box is freed.
    struct TapCtx {
        producer: SharedProducer,
        stop: Arc<AtomicBool>,
        resampler: rubato::FftFixedIn<f32>,
        pending: Vec<f32>,
        channels: usize,
        non_interleaved: bool,
    }

    /// Owns the whole capture chain; dropping it tears everything down in
    /// reverse order. Fields may be unset when construction failed partway.
    struct SystemTap {
        tap: ca::AudioObjectID,
        aggregate: Option<ca::AudioObjectID>,
        proc_id: ca::AudioDeviceIOProcID,
        started: bool,
        ctx: Option<Box<TapCtx>>,
    }

    impl Drop for SystemTap {
        fn drop(&mut self) {
            unsafe {
                if let Some(agg) = self.aggregate {
                    if self.started {
                        ca::AudioDeviceStop(agg, self.proc_id);
                    }
                    if self.proc_id.is_some() {
                        ca::AudioDeviceDestroyIOProcID(agg, self.proc_id);
                    }
                    ca::AudioHardwareDestroyAggregateDevice(agg);
                }
                AudioHardwareDestroyProcessTap(self.tap);
            }
            // self.ctx drops after this, once no callback can reference it.
        }
    }

    pub fn start(producer: SharedProducer, stop: Arc<AtomicBool>) -> Result<Box<dyn std::any::Any>> {
        let desc = tap_description()?;
        let tap_uid = tap_uuid_string(&desc);

        let mut tap: ca::AudioObjectID = 0;
        let status =
            unsafe { AudioHardwareCreateProcessTap(Id::as_ptr(&desc) as *mut AnyObject, &mut tap) };
        if status != 0 || tap == 0 {
            bail!("AudioHardwareCreateProcessTap failed: {}", fourcc(status));
        }
        // From here on, `guard` owns the tap; every early return cleans up.
        let mut guard = SystemTap {
            tap,
            aggregate: None,
            proc_id: None,
            started: false,
            ctx: None,
        };

        let asbd = tap_format(tap)?;
        let rate = asbd.mSampleRate.round() as usize;
        let channels = asbd.mChannelsPerFrame.max(1) as usize;
        if asbd.mFormatID != ca::kAudioFormatLinearPCM
            || asbd.mFormatFlags & ca::kAudioFormatFlagIsFloat == 0
            || asbd.mBitsPerChannel != 32
            || rate == 0
        {
            bail!(
                "tap delivers an unexpected format (id {:#x}, flags {:#x}, {} bit, {} Hz)",
                asbd.mFormatID,
                asbd.mFormatFlags,
                asbd.mBitsPerChannel,
                rate
            );
        }

        guard.aggregate = Some(create_aggregate(&tap_uid)?);
        let aggregate = guard.aggregate.unwrap();

        let mut ctx = Box::new(TapCtx {
            producer,
            stop,
            resampler: rubato::FftFixedIn::<f32>::new(rate, WHISPER_RATE, CHUNK, 2, 1)?,
            pending: Vec::with_capacity(CHUNK * 4),
            channels,
            non_interleaved: asbd.mFormatFlags & ca::kAudioFormatFlagIsNonInterleaved != 0,
        });

        let status = unsafe {
            ca::AudioDeviceCreateIOProcID(
                aggregate,
                Some(io_proc),
                &mut *ctx as *mut TapCtx as *mut c_void,
                &mut guard.proc_id,
            )
        };
        guard.ctx = Some(ctx);
        if status != 0 || guard.proc_id.is_none() {
            bail!("AudioDeviceCreateIOProcID failed: {}", fourcc(status));
        }

        let status = unsafe { ca::AudioDeviceStart(aggregate, guard.proc_id) };
        if status != 0 {
            bail!("AudioDeviceStart failed: {}", fourcc(status));
        }
        guard.started = true;

        log::info!(
            "system-audio tap running: {rate} Hz, {channels} ch ({})",
            if guard.ctx.as_ref().unwrap().non_interleaved { "planar" } else { "interleaved" }
        );
        Ok(Box::new(guard))
    }

    /// A CATapDescription for a global mono-mixdown tap: every process's
    /// output (none excluded), already mixed to one channel by the HAL.
    fn tap_description() -> Result<Id<AnyObject>> {
        let cls = AnyClass::get("CATapDescription").context(
            "CATapDescription unavailable — system-audio capture needs macOS 14.4 or newer",
        )?;
        unsafe {
            let empty: Id<AnyObject> = msg_send_id![class!(NSArray), array];
            let desc: Option<Id<AnyObject>> = msg_send_id![
                msg_send_id![cls, alloc],
                initMonoGlobalTapButExcludeProcesses: &*empty
            ];
            let desc = desc.context("CATapDescription init returned nil")?;
            // Private = invisible to other audio apps; playback stays audible.
            let _: () = msg_send![&*desc, setPrivate: Bool::YES];
            Ok(desc)
        }
    }

    fn tap_uuid_string(desc: &AnyObject) -> String {
        unsafe {
            let uuid: Id<AnyObject> = msg_send_id![desc, UUID];
            let s: Id<AnyObject> = msg_send_id![&*uuid, UUIDString];
            let utf8: *const std::os::raw::c_char = msg_send![&*s, UTF8String];
            std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned()
        }
    }

    fn tap_format(tap: ca::AudioObjectID) -> Result<ca::AudioStreamBasicDescription> {
        let addr = ca::AudioObjectPropertyAddress {
            mSelector: ca::kAudioTapPropertyFormat as u32,
            mScope: ca::kAudioObjectPropertyScopeGlobal as u32,
            mElement: ca::kAudioObjectPropertyElementMain as u32,
        };
        let mut asbd: ca::AudioStreamBasicDescription = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of::<ca::AudioStreamBasicDescription>() as u32;
        let status = unsafe {
            ca::AudioObjectGetPropertyData(
                tap,
                &addr,
                0,
                std::ptr::null(),
                &mut size,
                &mut asbd as *mut _ as *mut c_void,
            )
        };
        if status != 0 {
            bail!("reading tap format failed: {}", fourcc(status));
        }
        Ok(asbd)
    }

    /// A private aggregate device containing only the tap. Its input side is
    /// therefore exactly the tap's stream — no ambiguity about which buffer
    /// is system audio.
    fn create_aggregate(tap_uid: &str) -> Result<ca::AudioObjectID> {
        let key = |k: &'static [u8]| {
            CFString::from_static_string(std::str::from_utf8(&k[..k.len() - 1]).unwrap())
        };
        let sub_tap = CFDictionary::from_CFType_pairs(&[
            (key(ca::kAudioSubTapUIDKey).as_CFType(), CFString::new(tap_uid).as_CFType()),
            (
                key(ca::kAudioSubTapDriftCompensationKey).as_CFType(),
                CFBoolean::true_value().as_CFType(),
            ),
        ]);
        let desc = CFDictionary::from_CFType_pairs(&[
            (
                key(ca::kAudioAggregateDeviceUIDKey).as_CFType(),
                CFString::new(&format!("app.opengranola.tap.{tap_uid}")).as_CFType(),
            ),
            (
                key(ca::kAudioAggregateDeviceNameKey).as_CFType(),
                CFString::from_static_string("Open Granola system tap").as_CFType(),
            ),
            (
                key(ca::kAudioAggregateDeviceIsPrivateKey).as_CFType(),
                CFBoolean::true_value().as_CFType(),
            ),
            (
                key(ca::kAudioAggregateDeviceTapAutoStartKey).as_CFType(),
                CFBoolean::true_value().as_CFType(),
            ),
            (
                key(ca::kAudioAggregateDeviceTapListKey).as_CFType(),
                CFArray::from_CFTypes(&[sub_tap.as_CFType()]).as_CFType(),
            ),
        ]);
        let mut agg: ca::AudioObjectID = 0;
        let status = unsafe {
            ca::AudioHardwareCreateAggregateDevice(
                desc.as_concrete_TypeRef() as ca::CFDictionaryRef,
                &mut agg,
            )
        };
        if status != 0 || agg == 0 {
            bail!("AudioHardwareCreateAggregateDevice failed: {}", fourcc(status));
        }
        Ok(agg)
    }

    /// Runs on the HAL's realtime thread. Downmix to mono, resample to
    /// 16 kHz in CHUNK-sized frames (remainder carried across callbacks),
    /// push into the shared ring buffer — the same shape as the mic callback.
    unsafe extern "C" fn io_proc(
        _device: ca::AudioObjectID,
        _now: *const ca::AudioTimeStamp,
        in_input: *const ca::AudioBufferList,
        _input_time: *const ca::AudioTimeStamp,
        _out_output: *mut ca::AudioBufferList,
        _output_time: *const ca::AudioTimeStamp,
        client: *mut c_void,
    ) -> ca::OSStatus {
        use rubato::Resampler;

        let ctx = &mut *(client as *mut TapCtx);
        if ctx.stop.load(Ordering::Relaxed) || in_input.is_null() {
            return 0;
        }
        let abl = &*in_input;
        let n_bufs = abl.mNumberBuffers as usize;
        if n_bufs == 0 {
            return 0;
        }
        let bufs = std::slice::from_raw_parts(abl.mBuffers.as_ptr(), n_bufs);

        if ctx.non_interleaved {
            // One mono buffer per channel: average them frame by frame.
            let frames = bufs
                .iter()
                .map(|b| b.mDataByteSize as usize / 4)
                .min()
                .unwrap_or(0);
            for i in 0..frames {
                let mut acc = 0f32;
                let mut n = 0usize;
                for b in bufs {
                    if !b.mData.is_null() {
                        acc += *(b.mData as *const f32).add(i);
                        n += 1;
                    }
                }
                ctx.pending.push(if n > 0 { acc / n as f32 } else { 0.0 });
            }
        } else {
            // Interleaved frames in the first buffer.
            let b = &bufs[0];
            if b.mData.is_null() {
                return 0;
            }
            let samples =
                std::slice::from_raw_parts(b.mData as *const f32, b.mDataByteSize as usize / 4);
            let ch = ctx.channels.max(1);
            ctx.pending.extend(
                samples
                    .chunks(ch)
                    .map(|f| f.iter().sum::<f32>() / f.len().max(1) as f32),
            );
        }

        while ctx.pending.len() >= CHUNK {
            let frame: Vec<f32> = ctx.pending.drain(..CHUNK).collect();
            if let Ok(out) = ctx.resampler.process(&[frame], None) {
                let _ = ctx.producer.lock().push_slice(&out[0]); // drop on overflow, never block
            }
        }
        0
    }

    /// OSStatus rendered as its four-char code when printable ('!dat', 'who?').
    fn fourcc(status: ca::OSStatus) -> String {
        let b = (status as u32).to_be_bytes();
        if b.iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
            format!("{status} ('{}')", String::from_utf8_lossy(&b))
        } else {
            status.to_string()
        }
    }
}

#[cfg(windows)]
pub fn start(_producer: SharedProducer, stop: Arc<AtomicBool>) -> Result<Box<dyn std::any::Any>> {
    // STUB — mic-only on Windows for now. The real implementation is WASAPI
    // loopback: AUDCLNT_STREAMFLAGS_LOOPBACK on the default render endpoint,
    // event-driven capture on a dedicated MMCSS thread.
    let handle = std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    });
    Ok(Box::new(handle))
}

#[cfg(target_os = "linux")]
pub fn start(_producer: SharedProducer, stop: Arc<AtomicBool>) -> Result<Box<dyn std::any::Any>> {
    // STUB — mic-only on Linux for now. The real implementation is PipeWire:
    // pw_stream_connect(..., PW_DIRECTION_INPUT, monitor-of-default-sink ...).
    let handle = std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    });
    Ok(Box::new(handle))
}
