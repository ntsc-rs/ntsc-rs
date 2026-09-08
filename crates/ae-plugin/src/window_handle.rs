use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle,
    Win32WindowHandle, WindowHandle,
};

pub struct WindowAndDisplayHandle {
    raw_handle: Win32WindowHandle,
}

impl WindowAndDisplayHandle {
    /// Safety: The window handle must be valid for however long you intend to use it for
    #[allow(dead_code)]
    pub unsafe fn new(raw_handle: Win32WindowHandle) -> Self {
        Self { raw_handle }
    }
}

impl HasWindowHandle for WindowAndDisplayHandle {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(self.raw_handle)) })
    }
}

impl HasDisplayHandle for WindowAndDisplayHandle {
    fn display_handle(&self) -> Result<raw_window_handle::DisplayHandle<'_>, HandleError> {
        Ok(DisplayHandle::windows())
    }
}
