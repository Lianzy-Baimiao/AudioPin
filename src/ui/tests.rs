use super::*;
use std::sync::{Arc, Mutex};

// Exercise the real window procedure and controls without a guard thread,
// singleton mutex, audio changes, tray icon, or the user's configuration.
struct TestWindow {
    app: Box<RefCell<App>>,
    class: Vec<u16>,
    previous_dpi: DPI_AWARENESS_CONTEXT,
}

impl TestWindow {
    unsafe fn new() -> Self {
        let previous_dpi = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let class = wide(format!("AudioPin.LayoutTest.{}", GetCurrentThreadId()));
        let module = GetModuleHandleW(null());
        let definition = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: module,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        assert_ne!(RegisterClassW(&definition), 0);
        let app = Box::new(RefCell::new(App {
            hwnd: null_mut(),
            controls: HashMap::new(),
            fonts: vec![],
            config: Config::default(),
            shared: Arc::new(Mutex::new(guard::Snapshot::default())),
            tx: guard::Control::disconnected(),
            page: 0,
            scale: 1.0,
            physical_ids: vec![],
            output_ids: vec![],
            device_key: String::new(),
            tray_ok: true,
            taskbar_message: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            update_busy: false,
            last_events: String::new(),
            dirty: false,
        }));
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("AudioPin layout regression").as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            -10000,
            -10000,
            1230,
            947,
            null_mut(),
            null_mut(),
            module,
            (app.as_ref() as *const RefCell<App>).cast(),
        );
        assert!(!hwnd.is_null());
        {
            let mut state = app.borrow_mut();
            state.hwnd = hwnd;
            state.scale = GetDpiForWindow(hwnd) as f32 / 96.0;
            state.build();
            state.page(0);
            state.layout();
        }
        Self {
            app,
            class,
            previous_dpi,
        }
    }

    unsafe fn rectangles(&self) -> HashMap<i32, (i32, i32, i32, i32)> {
        self.app
            .borrow()
            .controls
            .iter()
            .map(|(id, control)| {
                let mut rect = RECT::default();
                assert_ne!(GetWindowRect(control.hwnd, &mut rect), 0);
                MapWindowPoints(
                    HWND_DESKTOP,
                    self.app.borrow().hwnd,
                    &mut rect as *mut _ as _,
                    2,
                );
                (*id, (rect.left, rect.top, rect.right, rect.bottom))
            })
            .collect()
    }

    unsafe fn dispatch_pending(&self) {
        let mut message = MSG::default();
        while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    unsafe fn reopen_after_maximized_close(&self, message: u32, argument: LPARAM) {
        let hwnd = self.app.borrow().hwnd;
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        self.dispatch_pending();
        let normal = self.rectangles();
        let mut normal_client = RECT::default();
        GetClientRect(hwnd, &mut normal_client);
        ShowWindow(hwnd, SW_MAXIMIZE);
        self.dispatch_pending();
        assert_ne!(IsZoomed(hwnd), 0);
        assert_ne!(self.rectangles(), normal);
        SendMessageW(hwnd, WM_CLOSE, 0, 0);
        assert_eq!(IsWindowVisible(hwnd), 0);
        SendMessageW(hwnd, message, 0, argument);
        self.dispatch_pending();
        assert_ne!(IsWindowVisible(hwnd), 0);
        let mut reopened_client = RECT::default();
        GetClientRect(hwnd, &mut reopened_client);
        assert_eq!(reopened_client.right, normal_client.right);
        assert_eq!(reopened_client.bottom, normal_client.bottom);

        // Returning to the same client size must return every control to its
        // original position, without manually calling the layout function.
        let reopened = self.rectangles();
        for (id, expected_rect) in normal {
            assert_eq!(
                reopened[&id], expected_rect,
                "control {id} kept stale coordinates after reopening from tray"
            );
        }
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        unsafe {
            let state = self.app.borrow();
            DestroyWindow(state.hwnd);
            for font in &state.fonts {
                DeleteObject(*font);
            }
            UnregisterClassW(self.class.as_ptr(), GetModuleHandleW(null()));
            SetThreadDpiAwarenessContext(self.previous_dpi);
        }
    }
}

#[test]
fn singleton_reopen_updates_control_layout() {
    unsafe {
        TestWindow::new().reopen_after_maximized_close(SHOW, 0);
    }
}

#[test]
fn tray_click_reopen_updates_control_layout() {
    unsafe {
        TestWindow::new().reopen_after_maximized_close(TRAY, WM_LBUTTONUP as LPARAM);
    }
}
