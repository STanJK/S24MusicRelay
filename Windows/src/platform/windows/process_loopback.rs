//! Raw Win32 process-loopback capture adapter, migrated from BE200AudioHub.
//!
//! L3 owns all COM/WASAPI ABI details. Callers receive copied Float32 stereo
//! frames only; no WASAPI buffer pointer escapes this module.
//!
//! No external Windows binding crate is used so the existing locked dependency
//! graph stays unchanged.

use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug)]
pub struct ProcessLoopbackPacket<'a> {
    pub frames: &'a [[f32; 2]],
    pub silent: bool,
    pub discontinuity: bool,
    pub device_position: u64,
    pub qpc_position: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessLoopbackCaptureStats {
    pub packets: u64,
    pub frames: u64,
    pub silent_frames: u64,
    pub discontinuities: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessLoopbackError {
    UnsupportedPlatform,
    UnsupportedWindowsBuild {
        major: u32,
        minor: u32,
        build: u32,
    },
    ThreadSpawn,
    StartupChannelClosed,
    ComInitialize(i32),
    CreateEvent(&'static str),
    ActivateAudioInterface(i32),
    ActivationWait(u32),
    ActivationResult(i32),
    AudioClient {
        operation: &'static str,
        hresult: i32,
    },
    WaitFailed(u32),
    CaptureClient {
        operation: &'static str,
        hresult: i32,
    },
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::{
        ffi::c_void,
        mem,
        ptr,
        sync::{
            atomic::{AtomicBool, AtomicU32, Ordering},
            mpsc,
            Arc,
        },
        thread,
    };

    type HResult = i32;
    type Handle = *mut c_void;

    const S_OK: HResult = 0;
    const E_NOINTERFACE: HResult = 0x8000_4002u32 as i32;

    const COINIT_MULTITHREADED: u32 = 0;

    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 258;
    const WAIT_FAILED: u32 = 0xffff_ffff;
    const CAPTURE_WAIT_MS: u32 = 50;
    const ACTIVATION_WAIT_MS: u32 = 10_000;

    const AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK: i32 = 1;
    const PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE: i32 = 1;
    const VT_BLOB: u16 = 65;

    const AUDCLNT_SHAREMODE_SHARED: i32 = 0;
    const AUDCLNT_STREAMFLAGS_LOOPBACK: u32 = 0x0002_0000;
    const AUDCLNT_STREAMFLAGS_EVENTCALLBACK: u32 = 0x0004_0000;
    const AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM: u32 = 0x8000_0000;
    const AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY: u32 = 0x1;
    const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 0x2;

    const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
    const SAMPLE_RATE_HZ: u32 = 48_000;
    const CHANNELS: u16 = 2;
    const BITS_PER_SAMPLE: u16 = 32;
    const BLOCK_ALIGN: u16 = 8;
    const AVG_BYTES_PER_SEC: u32 = 384_000;

    const VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK: &[u16] = &[
        'V' as u16, 'A' as u16, 'D' as u16, '\\' as u16,
        'P' as u16, 'r' as u16, 'o' as u16, 'c' as u16,
        'e' as u16, 's' as u16, 's' as u16, '_' as u16,
        'L' as u16, 'o' as u16, 'o' as u16, 'p' as u16,
        'b' as u16, 'a' as u16, 'c' as u16, 'k' as u16, 0,
    ];

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    const IID_IUNKNOWN: Guid = Guid {
        data1: 0,
        data2: 0,
        data3: 0,
        data4: [0xc0, 0, 0, 0, 0, 0, 0, 0x46],
    };
    const IID_IAGILE_OBJECT: Guid = Guid {
        data1: 0x94ea_2b94,
        data2: 0xe9cc,
        data3: 0x49e0,
        data4: [0xc0, 0xff, 0xee, 0x64, 0xca, 0x8f, 0x5b, 0x90],
    };
    const IID_IAUDIO_CLIENT: Guid = Guid {
        data1: 0x1cb9_ad4c,
        data2: 0xdbfa,
        data3: 0x4c32,
        data4: [0xb1, 0x78, 0xc2, 0xf5, 0x68, 0xa7, 0x03, 0xb2],
    };
    const IID_IAUDIO_CAPTURE_CLIENT: Guid = Guid {
        data1: 0xc8ad_bd64,
        data2: 0xe71e,
        data3: 0x48a0,
        data4: [0xa4, 0xde, 0x18, 0x5c, 0x39, 0x5c, 0xd3, 0x17],
    };
    const IID_IACTIVATE_AUDIO_INTERFACE_COMPLETION_HANDLER: Guid = Guid {
        data1: 0x41d9_49ab,
        data2: 0x9862,
        data3: 0x444a,
        data4: [0x80, 0xf6, 0xc2, 0x61, 0x33, 0x4d, 0xa5, 0xeb],
    };

    #[repr(C)]
    struct IUnknownVTable {
        query_interface: unsafe extern "system" fn(
            *mut c_void,
            *const Guid,
            *mut *mut c_void,
        ) -> HResult,
        add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
    }

    #[repr(C)]
    struct ActivateOperation {
        vtable: *const ActivateOperationVTable,
    }

    #[repr(C)]
    struct ActivateOperationVTable {
        base: IUnknownVTable,
        get_activate_result: unsafe extern "system" fn(
            *mut ActivateOperation,
            *mut HResult,
            *mut *mut c_void,
        ) -> HResult,
    }

    #[repr(C)]
    struct AudioClient {
        vtable: *const AudioClientVTable,
    }

    #[repr(C)]
    struct AudioClientVTable {
        base: IUnknownVTable,
        initialize: unsafe extern "system" fn(
            *mut AudioClient,
            i32,
            u32,
            i64,
            i64,
            *const WaveFormatEx,
            *const Guid,
        ) -> HResult,
        get_buffer_size:
            unsafe extern "system" fn(*mut AudioClient, *mut u32) -> HResult,
        get_stream_latency:
            unsafe extern "system" fn(*mut AudioClient, *mut i64) -> HResult,
        get_current_padding:
            unsafe extern "system" fn(*mut AudioClient, *mut u32) -> HResult,
        is_format_supported: unsafe extern "system" fn(
            *mut AudioClient,
            i32,
            *const WaveFormatEx,
            *mut *mut WaveFormatEx,
        ) -> HResult,
        get_mix_format: unsafe extern "system" fn(
            *mut AudioClient,
            *mut *mut WaveFormatEx,
        ) -> HResult,
        get_device_period: unsafe extern "system" fn(
            *mut AudioClient,
            *mut i64,
            *mut i64,
        ) -> HResult,
        start: unsafe extern "system" fn(*mut AudioClient) -> HResult,
        stop: unsafe extern "system" fn(*mut AudioClient) -> HResult,
        reset: unsafe extern "system" fn(*mut AudioClient) -> HResult,
        set_event_handle:
            unsafe extern "system" fn(*mut AudioClient, Handle) -> HResult,
        get_service: unsafe extern "system" fn(
            *mut AudioClient,
            *const Guid,
            *mut *mut c_void,
        ) -> HResult,
    }

    #[repr(C)]
    struct AudioCaptureClient {
        vtable: *const AudioCaptureClientVTable,
    }

    #[repr(C)]
    struct AudioCaptureClientVTable {
        base: IUnknownVTable,
        get_buffer: unsafe extern "system" fn(
            *mut AudioCaptureClient,
            *mut *mut u8,
            *mut u32,
            *mut u32,
            *mut u64,
            *mut u64,
        ) -> HResult,
        release_buffer:
            unsafe extern "system" fn(*mut AudioCaptureClient, u32) -> HResult,
        get_next_packet_size: unsafe extern "system" fn(
            *mut AudioCaptureClient,
            *mut u32,
        ) -> HResult,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct AudioClientProcessLoopbackParams {
        target_process_id: u32,
        process_loopback_mode: i32,
    }

    #[repr(C)]
    union AudioClientActivationParamsUnion {
        process_loopback_params: AudioClientProcessLoopbackParams,
    }

    #[repr(C)]
    struct AudioClientActivationParams {
        activation_type: i32,
        anonymous: AudioClientActivationParamsUnion,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Blob {
        size: u32,
        data: *mut u8,
    }

    #[repr(C)]
    union PropVariantData {
        blob: Blob,
    }

    #[repr(C)]
    struct PropVariant {
        vt: u16,
        reserved1: u16,
        reserved2: u16,
        reserved3: u16,
        data: PropVariantData,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct WaveFormatEx {
        format_tag: u16,
        channels: u16,
        samples_per_sec: u32,
        avg_bytes_per_sec: u32,
        block_align: u16,
        bits_per_sample: u16,
        extra_size: u16,
    }

    #[repr(C)]
    struct RtlOsVersionInfoW {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform_id: u32,
        service_pack: [u16; 128],
    }

    #[repr(C)]
    struct CompletionHandler {
        vtable: *const CompletionHandlerVTable,
        refs: AtomicU32,
        event: Handle,
    }

    #[repr(C)]
    struct CompletionHandlerVTable {
        base: IUnknownVTable,
        activate_completed: unsafe extern "system" fn(
            *mut CompletionHandler,
            *mut ActivateOperation,
        ) -> HResult,
    }

    static COMPLETION_HANDLER_VTABLE: CompletionHandlerVTable =
        CompletionHandlerVTable {
            base: IUnknownVTable {
                query_interface: completion_query_interface,
                add_ref: completion_add_ref,
                release: completion_release,
            },
            activate_completed: completion_activate_completed,
        };

    unsafe extern "system" fn completion_query_interface(
        this: *mut c_void,
        iid: *const Guid,
        out: *mut *mut c_void,
    ) -> HResult {
        if iid.is_null() || out.is_null() {
            return E_NOINTERFACE;
        }

        unsafe {
            *out = ptr::null_mut();
            let requested = *iid;
            if requested == IID_IUNKNOWN
                || requested == IID_IAGILE_OBJECT
                || requested == IID_IACTIVATE_AUDIO_INTERFACE_COMPLETION_HANDLER
            {
                *out = this;
                completion_add_ref(this);
                return S_OK;
            }
        }

        E_NOINTERFACE
    }

    unsafe extern "system" fn completion_add_ref(
        this: *mut c_void,
    ) -> u32 {
        let handler = unsafe { &*(this as *mut CompletionHandler) };
        handler.refs.fetch_add(1, Ordering::Relaxed) + 1
    }

    unsafe extern "system" fn completion_release(
        this: *mut c_void,
    ) -> u32 {
        let handler = unsafe { &*(this as *mut CompletionHandler) };
        let previous = handler.refs.fetch_sub(1, Ordering::AcqRel);
        let remaining = previous.saturating_sub(1);

        if previous == 1 {
            let boxed = unsafe {
                Box::from_raw(this as *mut CompletionHandler)
            };
            if !boxed.event.is_null() {
                unsafe {
                    let _ = CloseHandle(boxed.event);
                }
            }
            drop(boxed);
        }

        remaining
    }

    unsafe extern "system" fn completion_activate_completed(
        this: *mut CompletionHandler,
        _operation: *mut ActivateOperation,
    ) -> HResult {
        let handler = unsafe { &*this };
        if unsafe { SetEvent(handler.event) } == 0 {
            return -1;
        }
        S_OK
    }

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoInitializeEx(reserved: *const c_void, coinit: u32) -> HResult;
        fn CoUninitialize();
    }

    #[link(name = "mmdevapi")]
    unsafe extern "system" {
        fn ActivateAudioInterfaceAsync(
            device_interface_path: *const u16,
            iid: *const Guid,
            activation_params: *const PropVariant,
            completion_handler: *mut c_void,
            operation: *mut *mut ActivateOperation,
        ) -> HResult;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateEventW(
            event_attributes: *const c_void,
            manual_reset: i32,
            initial_state: i32,
            name: *const u16,
        ) -> Handle;
        fn SetEvent(event: Handle) -> i32;
        fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut RtlOsVersionInfoW) -> i32;
    }

    struct ComApartment;

    impl ComApartment {
        fn enter() -> Result<Self, ProcessLoopbackError> {
            let hr =
                unsafe { CoInitializeEx(ptr::null(), COINIT_MULTITHREADED) };
            if hr < 0 {
                return Err(ProcessLoopbackError::ComInitialize(hr));
            }
            Ok(Self)
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    struct OwnedHandle(Handle);

    impl OwnedHandle {
        fn create(label: &'static str) -> Result<Self, ProcessLoopbackError> {
            let handle = unsafe {
                CreateEventW(ptr::null(), 0, 0, ptr::null())
            };
            if handle.is_null() {
                return Err(ProcessLoopbackError::CreateEvent(label));
            }
            Ok(Self(handle))
        }

        fn raw(&self) -> Handle {
            self.0
        }
    }

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    struct CaptureSession {
        _com: ComApartment,
        audio: *mut AudioClient,
        capture: *mut AudioCaptureClient,
        sample_event: OwnedHandle,
        scratch: Vec<[f32; 2]>,
        started: bool,
    }

    impl Drop for CaptureSession {
        fn drop(&mut self) {
            unsafe {
                if self.started && !self.audio.is_null() {
                    let _ = ((*(*self.audio).vtable).stop)(self.audio);
                }
                release_capture(self.capture);
                release_audio(self.audio);
            }
        }
    }

    unsafe fn release_operation(value: *mut ActivateOperation) {
        if !value.is_null() {
            unsafe {
                ((*(*value).vtable).base.release)(value.cast());
            }
        }
    }

    unsafe fn release_audio(value: *mut AudioClient) {
        if !value.is_null() {
            unsafe {
                ((*(*value).vtable).base.release)(value.cast());
            }
        }
    }

    unsafe fn release_capture(value: *mut AudioCaptureClient) {
        if !value.is_null() {
            unsafe {
                ((*(*value).vtable).base.release)(value.cast());
            }
        }
    }

    fn check_hr(
        operation: &'static str,
        hr: HResult,
    ) -> Result<(), ProcessLoopbackError> {
        if hr < 0 {
            Err(ProcessLoopbackError::AudioClient {
                operation,
                hresult: hr,
            })
        } else {
            Ok(())
        }
    }

    fn windows_version() -> Option<(u32, u32, u32)> {
        let mut info: RtlOsVersionInfoW = unsafe { mem::zeroed() };
        info.size = mem::size_of::<RtlOsVersionInfoW>() as u32;
        let status = unsafe { RtlGetVersion(&mut info) };
        if status != 0 {
            return None;
        }
        Some((info.major, info.minor, info.build))
    }

    fn ensure_supported_windows() -> Result<(), ProcessLoopbackError> {
        if let Some((major, minor, build)) = windows_version() {
            if major < 10 || (major == 10 && build < 20_348) {
                return Err(
                    ProcessLoopbackError::UnsupportedWindowsBuild {
                        major,
                        minor,
                        build,
                    },
                );
            }
        }
        Ok(())
    }

    fn activate_audio_client(
        target_process_id: u32,
    ) -> Result<*mut AudioClient, ProcessLoopbackError> {
        let event = unsafe {
            CreateEventW(ptr::null(), 0, 0, ptr::null())
        };
        if event.is_null() {
            return Err(ProcessLoopbackError::CreateEvent(
                "activation completion",
            ));
        }

        let handler = Box::new(CompletionHandler {
            vtable: &COMPLETION_HANDLER_VTABLE,
            refs: AtomicU32::new(1),
            event,
        });
        let handler_ptr = Box::into_raw(handler);

        let mut activation = AudioClientActivationParams {
            activation_type:
                AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            anonymous: AudioClientActivationParamsUnion {
                process_loopback_params:
                    AudioClientProcessLoopbackParams {
                        target_process_id,
                        process_loopback_mode:
                            PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
                    },
            },
        };
        let params = PropVariant {
            vt: VT_BLOB,
            reserved1: 0,
            reserved2: 0,
            reserved3: 0,
            data: PropVariantData {
                blob: Blob {
                    size: mem::size_of::<AudioClientActivationParams>()
                        as u32,
                    data: (&mut activation as *mut AudioClientActivationParams)
                        .cast::<u8>(),
                },
            },
        };

        let mut operation: *mut ActivateOperation = ptr::null_mut();
        let hr = unsafe {
            ActivateAudioInterfaceAsync(
                VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK.as_ptr(),
                &IID_IAUDIO_CLIENT,
                &params,
                handler_ptr.cast(),
                &mut operation,
            )
        };
        if hr < 0 {
            unsafe {
                completion_release(handler_ptr.cast());
            }
            return Err(
                ProcessLoopbackError::ActivateAudioInterface(hr),
            );
        }

        let wait = unsafe {
            WaitForSingleObject(
                (*handler_ptr).event,
                ACTIVATION_WAIT_MS,
            )
        };
        if wait != WAIT_OBJECT_0 {
            unsafe {
                release_operation(operation);
                completion_release(handler_ptr.cast());
            }
            return Err(ProcessLoopbackError::ActivationWait(wait));
        }

        let mut activation_result = -1;
        let mut activated: *mut c_void = ptr::null_mut();
        let get_hr = unsafe {
            ((*(*operation).vtable).get_activate_result)(
                operation,
                &mut activation_result,
                &mut activated,
            )
        };

        unsafe {
            release_operation(operation);
            completion_release(handler_ptr.cast());
        }

        if get_hr < 0 {
            return Err(ProcessLoopbackError::ActivationResult(get_hr));
        }
        if activation_result < 0 {
            return Err(ProcessLoopbackError::ActivationResult(
                activation_result,
            ));
        }
        if activated.is_null() {
            return Err(ProcessLoopbackError::ActivationResult(-1));
        }

        Ok(activated.cast::<AudioClient>())
    }

    fn setup_capture(
        target_process_id: u32,
    ) -> Result<CaptureSession, ProcessLoopbackError> {
        ensure_supported_windows()?;
        let com = ComApartment::enter()?;
        let audio = activate_audio_client(target_process_id)?;

        let sample_event =
            match OwnedHandle::create("audio sample") {
                Ok(value) => value,
                Err(error) => {
                    unsafe { release_audio(audio) };
                    return Err(error);
                }
            };

        let format = WaveFormatEx {
            format_tag: WAVE_FORMAT_IEEE_FLOAT,
            channels: CHANNELS,
            samples_per_sec: SAMPLE_RATE_HZ,
            avg_bytes_per_sec: AVG_BYTES_PER_SEC,
            block_align: BLOCK_ALIGN,
            bits_per_sample: BITS_PER_SAMPLE,
            extra_size: 0,
        };
        let flags = AUDCLNT_STREAMFLAGS_LOOPBACK
            | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM;

        let init_hr = unsafe {
            ((*(*audio).vtable).initialize)(
                audio,
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                0,
                0,
                &format,
                ptr::null(),
            )
        };
        if let Err(error) = check_hr("Initialize", init_hr) {
            unsafe { release_audio(audio) };
            return Err(error);
        }

        let event_hr = unsafe {
            ((*(*audio).vtable).set_event_handle)(
                audio,
                sample_event.raw(),
            )
        };
        if let Err(error) = check_hr("SetEventHandle", event_hr) {
            unsafe { release_audio(audio) };
            return Err(error);
        }

        let mut buffer_frames = 0u32;
        let size_hr = unsafe {
            ((*(*audio).vtable).get_buffer_size)(
                audio,
                &mut buffer_frames,
            )
        };
        if let Err(error) = check_hr("GetBufferSize", size_hr) {
            unsafe { release_audio(audio) };
            return Err(error);
        }

        let mut stream_latency_hns = 0i64;
        let stream_latency_hr = unsafe {
            ((*(*audio).vtable).get_stream_latency)(
                audio,
                &mut stream_latency_hns,
            )
        };

        let mut default_period_hns = 0i64;
        let mut minimum_period_hns = 0i64;
        let device_period_hr = unsafe {
            ((*(*audio).vtable).get_device_period)(
                audio,
                &mut default_period_hns,
                &mut minimum_period_hns,
            )
        };

        let buffer_ms =
            f64::from(buffer_frames) * 1_000.0 / f64::from(SAMPLE_RATE_HZ);
        if stream_latency_hr >= 0 && device_period_hr >= 0 {
            println!(
                "[live][loopback] sample_rate_hz={} buffer_frames={} buffer_ms={:.3} stream_latency_hns={} stream_latency_ms={:.3} default_period_hns={} default_period_ms={:.3} minimum_period_hns={} minimum_period_ms={:.3}",
                SAMPLE_RATE_HZ,
                buffer_frames,
                buffer_ms,
                stream_latency_hns,
                stream_latency_hns as f64 / 10_000.0,
                default_period_hns,
                default_period_hns as f64 / 10_000.0,
                minimum_period_hns,
                minimum_period_hns as f64 / 10_000.0,
            );
        } else {
            println!(
                "[live][loopback] sample_rate_hz={} buffer_frames={} buffer_ms={:.3} stream_latency_hr=0x{:08X} device_period_hr=0x{:08X}",
                SAMPLE_RATE_HZ,
                buffer_frames,
                buffer_ms,
                stream_latency_hr as u32,
                device_period_hr as u32,
            );
        }

        let mut capture_raw: *mut c_void = ptr::null_mut();
        let service_hr = unsafe {
            ((*(*audio).vtable).get_service)(
                audio,
                &IID_IAUDIO_CAPTURE_CLIENT,
                &mut capture_raw,
            )
        };
        if let Err(error) = check_hr("GetService(IAudioCaptureClient)", service_hr)
        {
            unsafe { release_audio(audio) };
            return Err(error);
        }
        if capture_raw.is_null() {
            unsafe { release_audio(audio) };
            return Err(ProcessLoopbackError::AudioClient {
                operation: "GetService(IAudioCaptureClient)",
                hresult: -1,
            });
        }

        let capture = capture_raw.cast::<AudioCaptureClient>();
        let start_hr =
            unsafe { ((*(*audio).vtable).start)(audio) };
        if let Err(error) = check_hr("Start", start_hr) {
            unsafe {
                release_capture(capture);
                release_audio(audio);
            }
            return Err(error);
        }

        Ok(CaptureSession {
            _com: com,
            audio,
            capture,
            sample_event,
            scratch: vec![
                [0.0, 0.0];
                usize::try_from(buffer_frames.max(1)).unwrap_or(1)
            ],
            started: true,
        })
    }

    fn drain_packets<F>(
        session: &mut CaptureSession,
        callback: &mut F,
        stats: &mut ProcessLoopbackCaptureStats,
    ) -> Result<(), ProcessLoopbackError>
    where
        F: for<'a> FnMut(ProcessLoopbackPacket<'a>),
    {
        loop {
            let mut next_frames = 0u32;
            let hr = unsafe {
                ((*(*session.capture).vtable).get_next_packet_size)(
                    session.capture,
                    &mut next_frames,
                )
            };
            if hr < 0 {
                return Err(ProcessLoopbackError::CaptureClient {
                    operation: "GetNextPacketSize",
                    hresult: hr,
                });
            }
            if next_frames == 0 {
                return Ok(());
            }

            let mut data: *mut u8 = ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            let mut device_position = 0u64;
            let mut qpc_position = 0u64;

            let get_hr = unsafe {
                ((*(*session.capture).vtable).get_buffer)(
                    session.capture,
                    &mut data,
                    &mut frames,
                    &mut flags,
                    &mut device_position,
                    &mut qpc_position,
                )
            };
            if get_hr < 0 {
                return Err(ProcessLoopbackError::CaptureClient {
                    operation: "GetBuffer",
                    hresult: get_hr,
                });
            }

            let frame_count = frames as usize;
            if frame_count > session.scratch.len() {
                let release_hr = unsafe {
                    ((*(*session.capture).vtable).release_buffer)(
                        session.capture,
                        frames,
                    )
                };
                if release_hr < 0 {
                    return Err(ProcessLoopbackError::CaptureClient {
                        operation: "ReleaseBuffer",
                        hresult: release_hr,
                    });
                }
                return Err(ProcessLoopbackError::CaptureClient {
                    operation: "packet exceeds AudioClient buffer",
                    hresult: -1,
                });
            }

            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT != 0;
            let discontinuity =
                flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY != 0;

            if !silent {
                if data.is_null() {
                    let _ = unsafe {
                        ((*(*session.capture).vtable).release_buffer)(
                            session.capture,
                            frames,
                        )
                    };
                    return Err(ProcessLoopbackError::CaptureClient {
                        operation: "GetBuffer returned null PCM",
                        hresult: -1,
                    });
                }

                let samples = unsafe {
                    std::slice::from_raw_parts(
                        data.cast::<f32>(),
                        frame_count.saturating_mul(2),
                    )
                };
                for (index, frame) in
                    session.scratch[..frame_count].iter_mut().enumerate()
                {
                    *frame = [
                        samples[index * 2],
                        samples[index * 2 + 1],
                    ];
                }
            }

            let release_hr = unsafe {
                ((*(*session.capture).vtable).release_buffer)(
                    session.capture,
                    frames,
                )
            };
            if release_hr < 0 {
                return Err(ProcessLoopbackError::CaptureClient {
                    operation: "ReleaseBuffer",
                    hresult: release_hr,
                });
            }

            stats.packets = stats.packets.saturating_add(1);
            stats.frames =
                stats.frames.saturating_add(frame_count as u64);
            if silent {
                stats.silent_frames = stats
                    .silent_frames
                    .saturating_add(frame_count as u64);
            }
            if discontinuity {
                stats.discontinuities =
                    stats.discontinuities.saturating_add(1);
            }

            callback(ProcessLoopbackPacket {
                frames: &session.scratch[..frame_count],
                silent,
                discontinuity,
                device_position,
                qpc_position,
            });
        }
    }

    fn capture_thread<F>(
        target_process_id: u32,
        stop: Arc<AtomicBool>,
        startup: mpsc::SyncSender<Result<(), ProcessLoopbackError>>,
        mut callback: F,
    ) -> Result<ProcessLoopbackCaptureStats, ProcessLoopbackError>
    where
        F: for<'a> FnMut(ProcessLoopbackPacket<'a>),
    {
        let mut session = match setup_capture(target_process_id) {
            Ok(session) => session,
            Err(error) => {
                let _ = startup.send(Err(error.clone()));
                return Err(error);
            }
        };

        let _ = startup.send(Ok(()));
        let mut stats = ProcessLoopbackCaptureStats::default();

        while !stop.load(Ordering::Acquire) {
            let wait = unsafe {
                WaitForSingleObject(
                    session.sample_event.raw(),
                    CAPTURE_WAIT_MS,
                )
            };
            match wait {
                WAIT_OBJECT_0 => {
                    drain_packets(
                        &mut session,
                        &mut callback,
                        &mut stats,
                    )?;
                }
                WAIT_TIMEOUT => {}
                WAIT_FAILED => {
                    return Err(ProcessLoopbackError::WaitFailed(wait));
                }
                other => {
                    return Err(ProcessLoopbackError::WaitFailed(other));
                }
            }
        }

        Ok(stats)
    }

    pub struct ProcessLoopbackCapture {
        stop: Arc<AtomicBool>,
        join: Option<
            JoinHandle<
                Result<
                    ProcessLoopbackCaptureStats,
                    ProcessLoopbackError,
                >,
            >,
        >,
    }

    impl ProcessLoopbackCapture {
        pub fn start_excluding_process_tree<F>(
            target_process_id: u32,
            callback: F,
        ) -> Result<Self, ProcessLoopbackError>
        where
            F: for<'a> FnMut(ProcessLoopbackPacket<'a>)
                + Send
                + 'static,
        {
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = Arc::clone(&stop);
            let (startup_tx, startup_rx) = mpsc::sync_channel(1);

            let join = thread::Builder::new()
                .name("s24-process-loopback".into())
                .spawn(move || {
                    capture_thread(
                        target_process_id,
                        thread_stop,
                        startup_tx,
                        callback,
                    )
                })
                .map_err(|_| ProcessLoopbackError::ThreadSpawn)?;

            match startup_rx.recv() {
                Ok(Ok(())) => Ok(Self {
                    stop,
                    join: Some(join),
                }),
                Ok(Err(error)) => {
                    let _ = join.join();
                    Err(error)
                }
                Err(_) => {
                    let _ = join.join();
                    Err(ProcessLoopbackError::StartupChannelClosed)
                }
            }
        }

        pub fn shutdown(
            mut self,
        ) -> Result<ProcessLoopbackCaptureStats, ProcessLoopbackError>
        {
            self.stop.store(true, Ordering::Release);
            let Some(join) = self.join.take() else {
                return Ok(ProcessLoopbackCaptureStats::default());
            };
            join.join()
                .map_err(|_| ProcessLoopbackError::StartupChannelClosed)?
        }
    }

    impl Drop for ProcessLoopbackCapture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn completion_handler_advertises_agile_object() {
            let event = unsafe {
                CreateEventW(ptr::null(), 0, 0, ptr::null())
            };
            assert!(!event.is_null());

            let handler = Box::new(CompletionHandler {
                vtable: &COMPLETION_HANDLER_VTABLE,
                refs: AtomicU32::new(1),
                event,
            });
            let raw = Box::into_raw(handler);
            let mut out: *mut c_void = ptr::null_mut();

            let hr = unsafe {
                completion_query_interface(
                    raw.cast(),
                    &IID_IAGILE_OBJECT,
                    &mut out,
                )
            };
            assert_eq!(hr, S_OK);
            assert_eq!(out, raw.cast());

            unsafe {
                // QI adds one reference, then release both references.
                completion_release(out);
                completion_release(raw.cast());
            }
        }
    }

}

#[cfg(windows)]
pub use imp::ProcessLoopbackCapture;

#[cfg(not(windows))]
pub struct ProcessLoopbackCapture;

#[cfg(not(windows))]
impl ProcessLoopbackCapture {
    pub fn start_excluding_process_tree<F>(
        _target_process_id: u32,
        _callback: F,
    ) -> Result<Self, ProcessLoopbackError>
    where
        F: for<'a> FnMut(ProcessLoopbackPacket<'a>) + Send + 'static,
    {
        Err(ProcessLoopbackError::UnsupportedPlatform)
    }

    pub fn shutdown(
        self,
    ) -> Result<ProcessLoopbackCaptureStats, ProcessLoopbackError> {
        Err(ProcessLoopbackError::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_metadata_is_transport_neutral() {
        let frames = [[0.25f32, -0.25f32]; 2];
        let packet = ProcessLoopbackPacket {
            frames: &frames,
            silent: false,
            discontinuity: true,
            device_position: 10,
            qpc_position: 20,
        };
        assert_eq!(packet.frames.len(), 2);
        assert_eq!(packet.device_position, 10);
        assert_eq!(packet.qpc_position, 20);
        assert!(packet.discontinuity);
    }
}
