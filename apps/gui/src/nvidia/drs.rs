//! Windows bindings to the NVIDIA Driver Settings API.
//!
//! ABI layouts, function IDs and setting IDs are from the MIT-licensed NVIDIA
//! SDK headers (https://github.com/NVIDIA/nvapi).
//! The installed driver DLL is loaded through the Windows system interface.

// Only this module may call the Windows loader and the NVIDIA C ABI.
#![allow(unsafe_code)]

use std::{
    ffi::{OsStr, c_void},
    mem::{self, size_of},
    os::windows::ffi::OsStrExt,
    ptr::{self, NonNull},
};
use windows_sys::Win32::{
    Foundation::{FreeLibrary, HMODULE},
    System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
};

const STRING_LEN: usize = 2048;
type Wide = [u16; STRING_LEN];
type Handle = *mut c_void;
type Status = i32;
type Query = unsafe extern "C" fn(u32) -> *const c_void;

const SETTING_NOT_FOUND: Status = -160;
const EXECUTABLE_NOT_FOUND: Status = -166;
const CURRENT_PROFILE: u32 = 0;
const DWORD: u32 = 0;
pub(super) const VRR_OVERRIDE: u32 = 0x10a879cf;

#[derive(Debug)]
pub(super) enum Error {
    Unavailable,
    Driver(&'static str, Status),
    InvalidResponse,
    Path,
}

fn status(operation: &'static str, result: Status) -> Result<(), Error> {
    match result {
        0 => Ok(()),
        -2 | -3 | -6 | -104 => Err(Error::Unavailable),
        code => Err(Error::Driver(operation, code)),
    }
}

fn version<T>(revision: u32) -> u32 {
    (revision << 16) | size_of::<T>() as u32
}

fn wide(value: &OsStr) -> Result<Wide, Error> {
    let mut result = [0; STRING_LEN];
    for (index, unit) in value.encode_wide().enumerate() {
        if index >= STRING_LEN - 1 || unit == 0 {
            return Err(Error::Path);
        }
        result[index] = unit;
    }
    Ok(result)
}

#[repr(C)]
struct ProfileInfo {
    version: u32,
    name: Wide,
    gpu_support: u32,
    predefined: u32,
    applications: u32,
    settings: u32,
}

impl ProfileInfo {
    fn new(name: Wide) -> Self {
        Self {
            version: version::<Self>(1),
            name,
            gpu_support: 0,
            predefined: 0,
            applications: 0,
            settings: 0,
        }
    }
}

#[repr(C)]
struct Application {
    version: u32,
    predefined: u32,
    name: Wide,
    friendly_name: Wide,
    launcher: Wide,
    file_in_folder: Wide,
    flags: u32,
    command_line: Wide,
}

impl Application {
    fn new() -> Self {
        Self {
            version: version::<Self>(4),
            predefined: 0,
            name: [0; STRING_LEN],
            friendly_name: [0; STRING_LEN],
            launcher: [0; STRING_LEN],
            file_in_folder: [0; STRING_LEN],
            flags: 0,
            command_line: [0; STRING_LEN],
        }
    }
}

// The C unions are 4100 bytes each with 4-byte alignment. We only use their
// first word for DWORD values.
#[repr(C)]
struct Setting {
    version: u32,
    name: Wide,
    id: u32,
    kind: u32,
    location: u32,
    current_is_predefined: u32,
    predefined_is_valid: u32,
    predefined: [u32; 1025],
    current: [u32; 1025],
}

impl Setting {
    fn new(id: u32) -> Self {
        Self {
            version: version::<Self>(1),
            name: [0; STRING_LEN],
            id,
            kind: DWORD,
            location: CURRENT_PROFILE,
            current_is_predefined: 0,
            predefined_is_valid: 0,
            predefined: [0; 1025],
            current: [0; 1025],
        }
    }
}

const _: () = {
    assert!(size_of::<ProfileInfo>() == 4116);
    assert!(size_of::<Application>() == 20492);
    assert!(size_of::<Setting>() == 12320);
    assert!(mem::offset_of!(Setting, current) == 8220);
};

struct Library(HMODULE);

impl Library {
    fn open() -> Result<Self, Error> {
        let name = if cfg!(target_pointer_width = "64") {
            "nvapi64.dll"
        } else {
            "nvapi.dll"
        };
        let name = wide(OsStr::new(name))?;
        // SAFETY: name is a terminated UTF-16 string. Search only Windows System32.
        let module =
            unsafe { LoadLibraryExW(name.as_ptr(), ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
        if module.is_null() {
            Err(Error::Unavailable)
        } else {
            Ok(Self(module))
        }
    }

    fn query(&self) -> Result<Query, Error> {
        // SAFETY: the module is live and the export name is a terminated C string.
        let export = unsafe { GetProcAddress(self.0, c"nvapi_QueryInterface".as_ptr().cast()) }
            .ok_or(Error::Unavailable)?;
        // SAFETY: nvapi_QueryInterface has the C signature Query.
        Ok(unsafe { mem::transmute::<unsafe extern "system" fn() -> isize, Query>(export) })
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: this is the handle owned by this Library.
        unsafe { FreeLibrary(self.0) };
    }
}

macro_rules! interfaces {
    ($($name:ident: $id:literal => fn($($arg:ty),*);)+) => {
        struct Api {
            $($name: unsafe extern "C" fn($($arg),*) -> Status,)+
        }

        impl Api {
            fn load(library: &Library) -> Result<Self, Error> {
                let query = library.query()?;
                $(
                    // SAFETY: each ID is paired with the signature in NVIDIA's headers.
                    let address = unsafe { query($id) };
                    if address.is_null() { return Err(Error::Unavailable); }
                    // SAFETY: the address is non-null and the DLL remains loaded by Session.
                    let $name = unsafe { mem::transmute::<*const c_void, unsafe extern "C" fn($($arg),*) -> Status>(address) };
                )+
                Ok(Self { $($name),+ })
            }
        }
    };
}

interfaces! {
    initialize: 0x0150e828 => fn();
    unload: 0xd22bdd7e => fn();
    create_session: 0x0694d52e => fn(*mut Handle);
    destroy_session: 0xdad9cff8 => fn(Handle);
    load_settings: 0x375dbd6b => fn(Handle);
    save_settings: 0xfcbc7e14 => fn(Handle);
    find_application: 0xeee566b2 => fn(Handle, *const u16, *mut Handle, *mut Application);
    get_profile: 0x61cd6fd6 => fn(Handle, Handle, *mut ProfileInfo);
    create_profile: 0xcc176068 => fn(Handle, *mut ProfileInfo, *mut Handle);
    delete_profile: 0x17093206 => fn(Handle, Handle);
    create_application: 0x4347a9de => fn(Handle, Handle, *mut Application);
    get_setting: 0x73bf8338 => fn(Handle, Handle, u32, *mut Setting);
    set_setting: 0x577dd202 => fn(Handle, Handle, *mut Setting);
    delete_setting: 0xe4a26362 => fn(Handle, Handle, u32);
}

pub(super) struct Session {
    handle: Handle,
    api: Api,
    _library: Library,
}

#[derive(Clone, Copy)]
pub(super) struct Profile(NonNull<c_void>);

#[derive(Debug)]
pub(super) struct Info {
    pub(super) name: String,
    pub(super) applications: u32,
    pub(super) settings: u32,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Value {
    pub(super) effective: u32,
    pub(super) local: Option<u32>,
}

impl Session {
    pub(super) fn open() -> Result<Self, Error> {
        let library = Library::open()?;
        let api = Api::load(&library)?;
        // SAFETY: functions were resolved from the loaded NVIDIA driver.
        status("Initialize", unsafe { (api.initialize)() })?;
        let mut session = Self {
            handle: ptr::null_mut(),
            api,
            _library: library,
        };
        // SAFETY: handle points to writable storage of the documented size.
        status("CreateSession", unsafe {
            (session.api.create_session)(&mut session.handle)
        })?;
        if session.handle.is_null() {
            return Err(Error::InvalidResponse);
        }
        // SAFETY: CreateSession returned a live handle.
        status("LoadSettings", unsafe {
            (session.api.load_settings)(session.handle)
        })?;
        Ok(session)
    }

    pub(super) fn find(&self, executable: &OsStr) -> Result<Option<Profile>, Error> {
        let executable = wide(executable)?;
        let mut application = Application::new();
        let mut profile = ptr::null_mut();
        // SAFETY: the session is live and all buffers match the versioned C ABI.
        let result = unsafe {
            (self.api.find_application)(
                self.handle,
                executable.as_ptr(),
                &mut profile,
                &mut application,
            )
        };
        if result == EXECUTABLE_NOT_FOUND {
            return Ok(None);
        }
        status("FindApplicationByName", result)?;
        Ok(Some(Profile(
            NonNull::new(profile).ok_or(Error::InvalidResponse)?,
        )))
    }

    pub(super) fn info(&self, profile: Profile) -> Result<Info, Error> {
        let mut info = ProfileInfo::new([0; STRING_LEN]);
        // SAFETY: the profile belongs to this session and info has the documented layout.
        status("GetProfileInfo", unsafe {
            (self.api.get_profile)(self.handle, profile.0.as_ptr(), &mut info)
        })?;
        let end = info
            .name
            .iter()
            .position(|unit| *unit == 0)
            .ok_or(Error::InvalidResponse)?;
        let name = String::from_utf16(&info.name[..end]).map_err(|_| Error::InvalidResponse)?;
        Ok(Info {
            name,
            applications: info.applications,
            settings: info.settings,
        })
    }

    pub(super) fn create(&self, name: &str, executable: &OsStr) -> Result<Profile, Error> {
        let mut info = ProfileInfo::new(wide(OsStr::new(name))?);
        let mut handle = ptr::null_mut();
        // SAFETY: info and handle point to initialized versioned input and writable output.
        status("CreateProfile", unsafe {
            (self.api.create_profile)(self.handle, &mut info, &mut handle)
        })?;
        let profile = Profile(NonNull::new(handle).ok_or(Error::InvalidResponse)?);
        let mut application = Application::new();
        application.name = wide(executable)?;
        application.friendly_name = wide(OsStr::new("Ruscello"))?;
        // SAFETY: the profile belongs to this session and application matches the C ABI.
        status("CreateApplication", unsafe {
            (self.api.create_application)(self.handle, handle, &mut application)
        })?;
        Ok(profile)
    }

    pub(super) fn get(&self, profile: Profile, id: u32) -> Result<Option<Value>, Error> {
        let mut setting = Setting::new(id);
        // SAFETY: the profile belongs to this session and setting is a versioned output buffer.
        let result =
            unsafe { (self.api.get_setting)(self.handle, profile.0.as_ptr(), id, &mut setting) };
        if result == SETTING_NOT_FOUND {
            return Ok(None);
        }
        status("GetSetting", result)?;
        if setting.kind != DWORD {
            return Err(Error::InvalidResponse);
        }
        let effective = setting.current[0];
        let local = (setting.location == CURRENT_PROFILE && setting.current_is_predefined == 0)
            .then_some(effective);
        Ok(Some(Value { effective, local }))
    }

    pub(super) fn delete(&self, profile: Profile) -> Result<(), Error> {
        // SAFETY: the handle identifies a profile in this live session.
        status("DeleteProfile", unsafe {
            (self.api.delete_profile)(self.handle, profile.0.as_ptr())
        })
    }

    pub(super) fn set(&self, profile: Profile, id: u32, value: Option<u32>) -> Result<(), Error> {
        if let Some(value) = value {
            let mut setting = Setting::new(id);
            setting.current[0] = value;
            // SAFETY: the profile belongs to this session and the DWORD occupies the first union word.
            status("SetSetting", unsafe {
                (self.api.set_setting)(self.handle, profile.0.as_ptr(), &mut setting)
            })
        } else {
            // SAFETY: the profile belongs to this session and id identifies a DWORD setting.
            status("DeleteProfileSetting", unsafe {
                (self.api.delete_setting)(self.handle, profile.0.as_ptr(), id)
            })
        }
    }

    pub(super) fn save(&self) -> Result<(), Error> {
        // SAFETY: the driver session is live.
        status("SaveSettings", unsafe {
            (self.api.save_settings)(self.handle)
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: we own the session, if allocated, and balance successful Initialize.
        unsafe {
            if !self.handle.is_null() {
                (self.api.destroy_session)(self.handle);
            }
            (self.api.unload)();
        }
    }
}
