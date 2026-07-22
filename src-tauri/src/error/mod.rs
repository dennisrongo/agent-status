//! Shared error helpers. Internal APIs use `thiserror` enums; only the
//! `#[tauri::command]` boundary converts to `Result<T, String>`.

/// Convert any `Result<T, E: Display>` into `Result<T, String>` at the IPC boundary.
pub trait ResultExt<T> {
    fn into_string(self) -> Result<T, String>;
}

impl<T, E: std::fmt::Display> ResultExt<T> for Result<T, E> {
    fn into_string(self) -> Result<T, String> {
        self.map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_passes_through() {
        let r: Result<i32, &str> = Ok(42);
        assert_eq!(r.into_string(), Ok(42));
    }

    #[test]
    fn err_becomes_string() {
        let r: Result<i32, &str> = Err("boom");
        assert_eq!(r.into_string(), Err("boom".to_string()));
    }

    #[test]
    fn err_from_custom_type_becomes_display_string() {
        #[derive(Debug)]
        struct MyErr;
        impl std::fmt::Display for MyErr {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "custom error message")
            }
        }
        let r: Result<i32, MyErr> = Err(MyErr);
        assert_eq!(r.into_string(), Err("custom error message".to_string()));
    }
}
