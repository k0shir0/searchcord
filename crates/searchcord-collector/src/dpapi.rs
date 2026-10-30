//! Current-user DPAPI with no UI. Every Windows-owned allocation is freed.
use crate::credentials::CredentialError;
use std::{ffi::c_void, ptr};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    },
};
struct LocalMemory(*mut c_void);
impl Drop for LocalMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: DPAPI returns LocalAlloc memory; this owner frees it exactly once.
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}
pub fn transform(data: &[u8], decrypt: bool) -> Result<Vec<u8>, CredentialError> {
    let fail =
        || CredentialError("Windows could not unlock the credential vault for this user".into());
    let mut incoming_bytes = data.to_vec();
    let incoming = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len()).map_err(|_| fail())?,
        pbData: incoming_bytes.as_mut_ptr(),
    };
    let mut outgoing = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    let mut description = ptr::null_mut();
    let label: Vec<u16> = "Searchcord credentials\0".encode_utf16().collect();
    // SAFETY: All input buffers live through the synchronous call; optional entropy,
    // prompts and reserved arguments are null. Output pointers start null and are
    // exclusively owned below. No machine-wide flag is used.
    let ok = unsafe {
        if decrypt {
            CryptUnprotectData(
                &incoming,
                &mut description,
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut outgoing,
            )
        } else {
            CryptProtectData(
                &incoming,
                label.as_ptr(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut outgoing,
            )
        }
    };
    let _output_owner = LocalMemory(outgoing.pbData.cast());
    let _description_owner = LocalMemory(description.cast());
    if ok == 0 {
        return Err(fail());
    }
    if outgoing.cbData == 0 {
        return Ok(Vec::new());
    }
    if outgoing.pbData.is_null() {
        return Err(fail());
    }
    // SAFETY: A successful DPAPI call provides cbData readable bytes until LocalFree.
    Ok(unsafe { std::slice::from_raw_parts(outgoing.pbData, outgoing.cbData as usize) }.to_vec())
}
