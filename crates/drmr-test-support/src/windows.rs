//! Windows command-interpreter paths, used only for adversarial junction fixtures.
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::Path;

/// Preserve native UTF-16 while converting absolute paths to `mklink` syntax.
pub fn command_path(path: &Path) -> std::io::Result<OsString> {
    let absolute = std::path::absolute(path)?;
    let mut wide: Vec<_> = absolute
        .as_os_str()
        .encode_wide()
        .map(|unit| {
            if unit == u16::from(b'/') {
                u16::from(b'\\')
            } else {
                unit
            }
        })
        .collect();
    let unc: Vec<_> = "\\\\?\\UNC\\".encode_utf16().collect();
    let verbatim: Vec<_> = "\\\\?\\".encode_utf16().collect();
    if wide.starts_with(&unc) {
        wide.splice(..unc.len(), [u16::from(b'\\'), u16::from(b'\\')]);
    } else if wide.starts_with(&verbatim) {
        wide.drain(..verbatim.len());
    }
    Ok(OsString::from_wide(&wide))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_paths_use_native_separators_and_keep_utf16_units() {
        assert_eq!(
            command_path(Path::new(r"C:\fixture space\日本語/asset/reports")).unwrap(),
            OsString::from(r"C:\fixture space\日本語\asset\reports")
        );
        assert_eq!(
            command_path(Path::new(r"\\?\C:\fixture/asset")).unwrap(),
            OsString::from(r"C:\fixture\asset")
        );
        let mut units: Vec<_> = r"C:\fixture/".encode_utf16().collect();
        units.push(0xd800);
        let native = OsString::from_wide(&units);
        let actual: Vec<_> = command_path(Path::new(&native))
            .unwrap()
            .encode_wide()
            .collect();
        assert_eq!(actual.last(), Some(&0xd800));
        assert!(!actual.contains(&u16::from(b'/')));
    }
}
