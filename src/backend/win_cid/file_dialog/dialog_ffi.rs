use crate::FileDialog;

use std::{
    ffi::{c_void, OsStr, OsString},
    iter::once,
    ops::Deref,
    os::windows::{ffi::OsStrExt, prelude::OsStringExt},
    path::PathBuf,
    ptr,
};

use winapi::{
    shared::{
        guiddef::GUID, minwindef::LPVOID, ntdef::LPWSTR, winerror::HRESULT,
        wtypesbase::CLSCTX_INPROC_SERVER,
    },
    um::{
        combaseapi::{CoCreateInstance, CoTaskMemFree},
        shobjidl::{
            IFileDialog, IFileOpenDialog, IFileSaveDialog, FOS_ALLOWMULTISELECT, FOS_PICKFOLDERS,
        },
        shobjidl_core::{
            CLSID_FileOpenDialog, CLSID_FileSaveDialog, IShellItem, IShellItemArray,
            SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
        },
        shtypes::COMDLG_FILTERSPEC,
    },
    Interface,
};

#[cfg(feature = "parent")]
use raw_window_handle::RawWindowHandle;

use super::super::utils::ToResult;

fn to_os_string(s: &LPWSTR) -> OsString {
    let slice = unsafe {
        let mut len = 0;
        while *s.offset(len) != 0 {
            len += 1;
        }
        std::slice::from_raw_parts(*s, len as usize)
    };
    OsStringExt::from_wide(slice)
}

pub struct IDialog(pub *mut IFileDialog, Option<*mut c_void>);

impl IDialog {
    fn new_file_dialog(class: &GUID, id: &GUID) -> Result<*mut IFileDialog, HRESULT> {
        let mut dialog: *mut IFileDialog = ptr::null_mut();

        unsafe {
            CoCreateInstance(
                class,
                ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                id,
                &mut dialog as *mut *mut IFileDialog as *mut LPVOID,
            )
            .check()?;
        }

        Ok(dialog)
    }

    fn new_open_dialog(opt: &FileDialog) -> Result<Self, HRESULT> {
        let ptr = Self::new_file_dialog(&CLSID_FileOpenDialog, &IFileOpenDialog::uuidof())?;
        #[cfg(feature = "parent")]
        let parent = match opt.parent {
            Some(RawWindowHandle::Windows(handle)) => Some(handle.hwnd),
            None => None,
            _ => unreachable!("unsupported window handle, expected: Windows"),
        };
        #[cfg(not(feature = "parent"))]
        let parent = None;
        Ok(Self(ptr, parent))
    }

    fn new_save_dialog(opt: &FileDialog) -> Result<Self, HRESULT> {
        let ptr = Self::new_file_dialog(&CLSID_FileSaveDialog, &IFileSaveDialog::uuidof())?;
        #[cfg(feature = "parent")]
        let parent = match opt.parent {
            Some(RawWindowHandle::Windows(handle)) => Some(handle.hwnd),
            None => None,
            _ => unreachable!("unsupported window handle, expected: Windows"),
        };
        #[cfg(not(feature = "parent"))]
        let parent = None;
        Ok(Self(ptr, parent))
    }

    fn add_filters(&self, filters: &[crate::dialog::Filter]) -> Result<(), HRESULT> {
        if let Some(first_filter) = filters.first() {
            if let Some(first_extension) = first_filter.extensions.first() {
                let extension: Vec<u16> = first_extension.encode_utf16().chain(Some(0)).collect();
                unsafe {
                    (*self.0).SetDefaultExtension(extension.as_ptr()).check()?;
                }
            }
        }

        let f_list = {
            let mut f_list = Vec::new();

            for f in filters.iter() {
                let name: Vec<u16> = OsStr::new(&f.name).encode_wide().chain(once(0)).collect();
                let ext_string = f
                    .extensions
                    .iter()
                    .map(|item| format!("*.{}", item))
                    .collect::<Vec<_>>()
                    .join(";");

                let ext: Vec<u16> = OsStr::new(&ext_string)
                    .encode_wide()
                    .chain(once(0))
                    .collect();

                f_list.push((name, ext));
            }
            f_list
        };

        let spec: Vec<_> = f_list
            .iter()
            .map(|(name, ext)| COMDLG_FILTERSPEC {
                pszName: name.as_ptr(),
                pszSpec: ext.as_ptr(),
            })
            .collect();

        unsafe {
            if !spec.is_empty() {
                (*self.0)
                    .SetFileTypes(spec.len() as _, spec.as_ptr())
                    .check()?;
            }
        }
        Ok(())
    }

    fn set_path(&self, path: &Option<PathBuf>) -> Result<(), HRESULT> {
        if let Some(path) = path {
            if let Some(path) = path.to_str() {
                let wide_path: Vec<u16> = OsStr::new(path).encode_wide().chain(once(0)).collect();

                unsafe {
                    let mut item: *mut IShellItem = ptr::null_mut();
                    SHCreateItemFromParsingName(
                        wide_path.as_ptr(),
                        ptr::null_mut(),
                        &IShellItem::uuidof(),
                        &mut item as *mut *mut IShellItem as *mut *mut _,
                    )
                    .check()?;

                    // For some reason SetDefaultFolder(), does not guarantees default path, so we use SetFolder
                    (*self.0).SetFolder(item).check()?;
                }
            }
        }
        Ok(())
    }

    fn set_file_name(&self, file_name: &Option<String>) -> Result<(), HRESULT> {
        if let Some(path) = file_name {
            let wide_path: Vec<u16> = OsStr::new(path).encode_wide().chain(once(0)).collect();

            unsafe {
                (*self.0).SetFileName(wide_path.as_ptr()).check()?;
            }
        }
        Ok(())
    }

    fn set_title(&self, title: &Option<String>) -> Result<(), HRESULT> {
        if let Some(title) = title {
            let wide_title: Vec<u16> = OsStr::new(title).encode_wide().chain(once(0)).collect();

            unsafe {
                (*self.0).SetTitle(wide_title.as_ptr()).check()?;
            }
        }
        Ok(())
    }

    pub fn get_results(&self) -> Result<Vec<PathBuf>, HRESULT> {
        unsafe {
            let mut res_items: *mut IShellItemArray = ptr::null_mut();
            (*(self.0 as *mut IFileOpenDialog))
                .GetResults(&mut res_items)
                .check()?;

            let items = &*res_items;
            let mut count = 0;
            items.GetCount(&mut count);
            let mut paths = Vec::new();
            for id in 0..count {
                let mut res_item: *mut IShellItem = ptr::null_mut();
                items.GetItemAt(id, &mut res_item).check()?;
                let mut display_name: LPWSTR = ptr::null_mut();
                (*res_item)
                    .GetDisplayName(SIGDN_FILESYSPATH, &mut display_name)
                    .check()?;
                let filename = to_os_string(&display_name);
                CoTaskMemFree(display_name as LPVOID);
                let path = PathBuf::from(filename);
                paths.push(path);
            }
            items.Release();

            Ok(paths)
        }
    }

    pub fn get_result(&self) -> Result<PathBuf, HRESULT> {
        let mut res_item: *mut IShellItem = ptr::null_mut();
        unsafe {
            (*self.0).GetResult(&mut res_item).check()?;

            let mut display_name: LPWSTR = ptr::null_mut();

            (*res_item)
                .GetDisplayName(SIGDN_FILESYSPATH, &mut display_name)
                .check()?;

            let filename = to_os_string(&display_name);
            CoTaskMemFree(display_name as LPVOID);

            Ok(PathBuf::from(filename))
        }
    }

    pub fn show(&self) -> Result<(), HRESULT> {
        unsafe {
            self.Show(self.1.unwrap_or_else(|| ptr::null_mut()) as _)
                .check()?
        };
        Ok(())
    }
}

impl IDialog {
    pub fn build_pick_file(opt: &FileDialog) -> Result<Self, HRESULT> {
        let dialog = IDialog::new_open_dialog(opt)?;

        dialog.add_filters(&opt.filters)?;
        dialog.set_path(&opt.starting_directory)?;
        dialog.set_file_name(&opt.file_name)?;
        dialog.set_title(&opt.title)?;

        Ok(dialog)
    }

    pub fn build_save_file(opt: &FileDialog) -> Result<Self, HRESULT> {
        let dialog = IDialog::new_save_dialog(opt)?;

        dialog.add_filters(&opt.filters)?;
        dialog.set_path(&opt.starting_directory)?;
        dialog.set_file_name(&opt.file_name)?;
        dialog.set_title(&opt.title)?;

        Ok(dialog)
    }

    pub fn build_pick_folder(opt: &FileDialog) -> Result<Self, HRESULT> {
        let dialog = IDialog::new_open_dialog(opt)?;

        dialog.set_path(&opt.starting_directory)?;
        dialog.set_title(&opt.title)?;

        unsafe {
            dialog.SetOptions(FOS_PICKFOLDERS).check()?;
        }

        Ok(dialog)
    }

    pub fn build_pick_files(opt: &FileDialog) -> Result<Self, HRESULT> {
        let dialog = IDialog::new_open_dialog(opt)?;

        dialog.add_filters(&opt.filters)?;
        dialog.set_path(&opt.starting_directory)?;
        dialog.set_file_name(&opt.file_name)?;
        dialog.set_title(&opt.title)?;

        unsafe {
            dialog.SetOptions(FOS_ALLOWMULTISELECT).check()?;
        }

        Ok(dialog)
    }
}

impl Deref for IDialog {
    type Target = IFileDialog;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.0 }
    }
}

impl Drop for IDialog {
    fn drop(&mut self) {
        unsafe { (*(self.0 as *mut IFileDialog)).Release() };
    }
}
