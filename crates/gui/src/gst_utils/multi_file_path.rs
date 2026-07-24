use std::path::{Path, PathBuf};

/// Convert an output file path, with the sequence number specified as a series of "#" placeholders, to a printf-style
/// format string. Handles escaping percent signs and prevents any potential pitfalls from passing a user-supplied
/// string directly to printf (yes, GStreamer actually does this).
pub fn format_path_for_multi_file(path: impl AsRef<Path>) -> PathBuf {
    let mut sequence_path = Vec::<u8>::with_capacity(path.as_ref().as_os_str().len());
    let src_path_bytes = path.as_ref().as_os_str().as_encoded_bytes();
    let mut i = 0;
    let mut added_sequence_number = false;
    while i < src_path_bytes.len() {
        let path_char = src_path_bytes[i];
        // If it's a "%" character, double it
        if path_char == b'%' {
            sequence_path.push(b'%');
        }

        // We can only use the placeholder once because it's only passed once into printf
        if path_char == b'#' && !added_sequence_number {
            added_sequence_number = true;
            let mut num_digits = 0;
            while src_path_bytes.get(i) == Some(&b'#') {
                num_digits += 1;
                i += 1;
            }
            sequence_path.extend(*b"%0");
            sequence_path.extend(num_digits.to_string().bytes());
            sequence_path.push(b'd');
        } else {
            sequence_path.push(src_path_bytes[i]);
            i += 1;
        }
    }

    if !added_sequence_number {
        // If no sequence number was specified by the user, insert one before the file extension
        let last_ext = sequence_path
            .iter()
            .rposition(|c| *c == b'.')
            .unwrap_or(sequence_path.len());
        sequence_path.splice(last_ext..last_ext, *b"_%d");
    }

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(sequence_path))
    }

    #[cfg(not(unix))]
    PathBuf::from(String::from_utf8(sequence_path).unwrap())
}
