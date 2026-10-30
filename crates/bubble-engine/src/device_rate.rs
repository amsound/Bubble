//! Windows: the rate the system runs an output device at, and setting it.
//!
//! In shared mode Windows mixes every app's audio at the device's own format (its
//! "Default format" in Sound settings) and converts anything else to it. Bubble mixes
//! at its own rate (48 kHz first, 44.1 kHz second) after converting its files to that
//! rate, so a device at another rate means a second conversion in Windows. To avoid
//! it Bubble sets the device's format to its own rate, as the Sound control panel
//! does, through the interface the panel uses (IPolicyConfig). It is undocumented, so
//! every step is checked; when it fails the device is left as it was and the
//! interface warns.

use std::ffi::c_void;

use windows_sys::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows_sys::core::GUID;

// CPolicyConfigClient and IPolicyConfig (Windows 7 and later).
const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
const IID_POLICY_CONFIG: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);

/// The start of IPolicyConfig's table, as far as Bubble uses it.
#[repr(C)]
struct PolicyConfigVtbl {
    _query_interface: usize,
    _add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_mix_format: unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut u8) -> i32,
    get_device_format: unsafe extern "system" fn(*mut c_void, *const u16, i32, *mut *mut u8) -> i32,
    _reset_device_format: usize,
    set_device_format: unsafe extern "system" fn(*mut c_void, *const u16, *mut u8, *mut u8) -> i32,
}

/// COM for this thread while it lives (if the thread had it already, either way, it
/// is left as it was).
struct Com(bool);

impl Com {
    fn new() -> Com {
        // SAFETY: a plain call; S_OK and S_FALSE are balanced by CoUninitialize.
        let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) };
        Com(hr >= 0)
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    }
}

/// A WAVEFORMATEX (or WAVEFORMATEXTENSIBLE) Windows allocated.
struct Format(*mut u8);

impl Format {
    fn rate(&self) -> u32 {
        // SAFETY: nSamplesPerSec, at offset 4 of the (packed) WAVEFORMATEX.
        unsafe { std::ptr::read_unaligned(self.0.add(4) as *const u32) }
    }

    /// Changes the rate, and the bytes per second that follow from it.
    fn set_rate(&mut self, rate: u32) {
        // SAFETY: nSamplesPerSec (offset 4), nAvgBytesPerSec (8), nBlockAlign (12).
        unsafe {
            let align = std::ptr::read_unaligned(self.0.add(12) as *const u16) as u32;
            std::ptr::write_unaligned(self.0.add(4) as *mut u32, rate);
            std::ptr::write_unaligned(self.0.add(8) as *mut u32, rate * align);
        }
    }
}

impl Drop for Format {
    fn drop(&mut self) {
        // SAFETY: allocated by the audio service with CoTaskMemAlloc.
        unsafe { CoTaskMemFree(self.0 as *const c_void) };
    }
}

struct PolicyConfig(*mut c_void);

impl PolicyConfig {
    fn new() -> Result<PolicyConfig, String> {
        let mut p: *mut c_void = std::ptr::null_mut();
        // SAFETY: creating a COM object by its class and interface ids.
        let hr = unsafe {
            CoCreateInstance(
                &CLSID_POLICY_CONFIG,
                std::ptr::null_mut(),
                CLSCTX_ALL,
                &IID_POLICY_CONFIG,
                &mut p,
            )
        };
        if hr < 0 || p.is_null() {
            return Err(format!(
                "the Windows audio settings are not available ({hr:#x})"
            ));
        }
        Ok(PolicyConfig(p))
    }

    fn vtbl(&self) -> &PolicyConfigVtbl {
        // SAFETY: a COM object's first field is its method table.
        unsafe { &**(self.0 as *const *const PolicyConfigVtbl) }
    }

    fn mix_format(&self, id: &[u16]) -> Option<Format> {
        let mut f: *mut u8 = std::ptr::null_mut();
        // SAFETY: the device id is NUL-terminated; Windows allocates the format.
        let hr = unsafe { (self.vtbl().get_mix_format)(self.0, id.as_ptr(), &mut f) };
        (hr >= 0 && !f.is_null()).then_some(Format(f))
    }

    fn device_format(&self, id: &[u16]) -> Option<Format> {
        let mut f: *mut u8 = std::ptr::null_mut();
        // SAFETY: as above; 0 asks for the current format, not the driver's default.
        let hr = unsafe { (self.vtbl().get_device_format)(self.0, id.as_ptr(), 0, &mut f) };
        (hr >= 0 && !f.is_null()).then_some(Format(f))
    }
}

impl Drop for PolicyConfig {
    fn drop(&mut self) {
        // SAFETY: releases the reference CoCreateInstance gave.
        unsafe { (self.vtbl().release)(self.0) };
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The device's two rates, by its endpoint id as cpal gives it: its format (what Sound
/// settings shows) and the format Windows mixes at. They can differ.
pub fn device_rates(id: &str) -> Option<(u32, u32)> {
    let _com = Com::new();
    let pc = PolicyConfig::new().ok()?;
    let id = wide(id);
    Some((pc.device_format(&id)?.rate(), pc.mix_format(&id)?.rate()))
}

/// Sets the device's format and mix format to `rate`, keeping its channels and sample
/// size, and returns the two rates Windows then reports. Other apps playing on the
/// device are interrupted briefly while it changes.
pub fn set_device_rate(id: &str, rate: u32) -> Result<(u32, u32), String> {
    let _com = Com::new();
    let pc = PolicyConfig::new()?;
    let id = wide(id);
    let mut device = pc
        .device_format(&id)
        .ok_or("Windows did not give the device's format")?;
    let mut mix = pc
        .mix_format(&id)
        .ok_or("Windows did not give the device's mix format")?;
    device.set_rate(rate);
    mix.set_rate(rate);
    // SAFETY: two valid formats Windows allocated, changed only in their rate.
    let hr = unsafe { (pc.vtbl().set_device_format)(pc.0, id.as_ptr(), device.0, mix.0) };
    if hr < 0 {
        return Err(format!(
            "Windows refused {rate} Hz for this device ({hr:#x})"
        ));
    }
    let now = || -> Option<(u32, u32)> {
        Some((pc.device_format(&id)?.rate(), pc.mix_format(&id)?.rate()))
    };
    // The audio service applies it a moment later.
    for _ in 0..10 {
        if now() == Some((rate, rate)) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    now().ok_or_else(|| "Windows did not give the device's format".into())
}
