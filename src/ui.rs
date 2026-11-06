use crate::{
    config::{data_dir, Config},
    guard::{self, Command, Shared},
    platform::{self, wide},
    updates,
};
use anyhow::{bail, Result};
use std::{
    cell::RefCell,
    collections::HashMap,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{LibraryLoader::*, Threading::*},
    UI::{Controls::*, HiDpi::*, Shell::*, WindowsAndMessaging::*},
};

const CLASS: &str = "AudioPin.MainWindow.v1";
// Win32 MAKEINTRESOURCE: low word is a resource identifier, not a pointer.
fn resource(id: u16) -> *const u16 {
    id as usize as *const u16
}
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
const TRAY: u32 = WM_APP + 1;
const SHOW: u32 = WM_APP + 2;
const PAUSE: i32 = 201;
const CHECK: i32 = 202;
const SAVE: i32 = 203;
const STATUS: i32 = 210;
const DETAIL: i32 = 211;
const COUNTS: i32 = 212;
const EVENTS: i32 = 213;
const PHYSICAL: i32 = 220;
const OUTPUT: i32 = 221;
const PIN_DEFAULT: i32 = 222;
const PIN_COMMS: i32 = 223;
const AMD: i32 = 224;
const PIN_APPS: i32 = 230;
const APP_NAMES: i32 = 231;
const APP_STATUS: i32 = 232;
const POLL: i32 = 240;
const RETRY: i32 = 241;
const NOTIFY: i32 = 242;
const COOLDOWN: i32 = 243;
const AUTOSTART: i32 = 244;
const MINIMIZED: i32 = 245;
const CLOSE_TRAY: i32 = 246;
const RESTART_AMD: i32 = 247;
const RELOAD_AMD_PANEL: i32 = 248;
const REPO: i32 = 250;
const AUTH: i32 = 251;
const AUTO_UPDATE: i32 = 252;
const UPDATE: i32 = 253;
const UPDATE_STATUS: i32 = 254;
const RELEASES: i32 = 255;
const PROJECT: i32 = 256;
const OPEN_CONFIG: i32 = 260;
const RELOAD: i32 = 261;
const OPEN_LOG: i32 = 262;
const DIAG: i32 = 263;
const EXIT: i32 = 299;
const SOURCE_SUMMARY: i32 = 270;
const TARGET_SUMMARY: i32 = 271;
const AMD_SUMMARY: i32 = 272;
const ROUTE_SUMMARY: i32 = 273;
const FOOTER: i32 = 274;
const BG: COLORREF = rgb(246, 248, 251);
const SIDE: COLORREF = rgb(237, 241, 247);
const INK: COLORREF = rgb(28, 39, 55);
const MUTED: COLORREF = rgb(99, 113, 132);
const ACCENT: COLORREF = rgb(36, 99, 218);
const WHITE: COLORREF = rgb(255, 255, 255);
const fn rgb(r: u32, g: u32, b: u32) -> COLORREF {
    r | (g << 8) | (b << 16)
}

struct Control {
    hwnd: HWND,
    page: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    stretch: bool,
}
struct App {
    hwnd: HWND,
    controls: HashMap<i32, Control>,
    fonts: Vec<HFONT>,
    config: Config,
    shared: Shared,
    tx: guard::Control,
    page: i32,
    scale: f32,
    physical_ids: Vec<String>,
    output_ids: Vec<String>,
    device_key: String,
    tray_ok: bool,
    taskbar_message: u32,
    update_busy: bool,
    last_events: String,
    dirty: bool,
}
pub fn run(config: Config, background: bool) -> Result<()> {
    unsafe {
        let instance = platform::handle(CreateMutexW(
            null(),
            0,
            wide(r"Local\AudioPin.Singleton.v1").as_ptr(),
        ))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let existing = FindWindowW(wide(CLASS).as_ptr(), null());
            if !existing.is_null() {
                PostMessageW(existing, SHOW, 0, 0);
            }
            return Ok(());
        }
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let common = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES,
        };
        InitCommonControlsEx(&common);
        let module = GetModuleHandleW(null());
        let icon = LoadIconW(module, resource(1));
        let class_name = wide(CLASS);
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: module,
            lpszClassName: class_name.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
            hIcon: icon,
            hIconSm: icon,
            ..std::mem::zeroed()
        };
        if RegisterClassExW(&class) == 0 {
            bail!("无法注册窗口类：{}", std::io::Error::last_os_error());
        }
        let (tx, shared, worker) = guard::start(config.clone());
        let app = Box::new(RefCell::new(App {
            hwnd: null_mut(),
            controls: HashMap::new(),
            fonts: vec![],
            config,
            shared,
            tx,
            page: 0,
            scale: 1.0,
            physical_ids: vec![],
            output_ids: vec![],
            device_key: String::new(),
            tray_ok: false,
            taskbar_message: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            update_busy: false,
            last_events: String::new(),
            dirty: false,
        }));
        let scale = GetDpiForSystem() as f32 / 96.0;
        let hwnd = CreateWindowExW(
            0,
            wide(CLASS).as_ptr(),
            wide("AudioPin · 音频守护").as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            (984.0 * scale) as i32,
            (758.0 * scale) as i32,
            null_mut(),
            null_mut(),
            module,
            (app.as_ref() as *const RefCell<App>).cast(),
        );
        let mut state = app.borrow_mut();
        if hwnd.is_null() {
            let _ = state.tx.send(Command::Stop);
            let _ = worker.join();
            bail!("创建窗口失败");
        }
        state.hwnd = hwnd;
        state.scale = GetDpiForWindow(hwnd) as f32 / 96.0;
        state.build();
        state.tray_ok = state.tray(NIM_ADD, None);
        state.load_fields();
        state.page(0);
        state.layout();
        SetTimer(hwnd, 1, 500, None);
        if !background || !state.config.start_minimized || !state.tray_ok {
            ShowWindow(hwnd, SW_SHOW);
        }
        if state.config.auto_check_updates {
            state.check_update();
        }
        drop(state);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let state = app.borrow();
        state.tray(NIM_DELETE, None);
        let _ = state.tx.send(Command::Stop);
        let _ = worker.join();
        for f in &state.fonts {
            DeleteObject(*f);
        }
        drop(instance);
    }
    Ok(())
}
impl App {
    #[allow(clippy::too_many_arguments)]
    unsafe fn add(
        &mut self,
        id: i32,
        page: i32,
        class: &str,
        text: &str,
        style: u32,
        rect: (i32, i32, i32, i32),
        stretch: bool,
    ) {
        let (x, y, w, h) = rect;
        let hwnd = CreateWindowExW(
            if class == "EDIT" && style & ES_READONLY as u32 == 0 {
                WS_EX_CLIENTEDGE
            } else {
                0
            },
            wide(class).as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            x,
            y,
            w,
            h,
            self.hwnd,
            id as HMENU,
            GetModuleHandleW(null()),
            null(),
        );
        SendMessageW(hwnd, WM_SETFONT, self.fonts[0] as usize, 1);
        self.controls.insert(
            id,
            Control {
                hwnd,
                page,
                x,
                y,
                w,
                h,
                stretch,
            },
        );
    }
    unsafe fn label(&mut self, id: i32, page: i32, text: &str, rect: (i32, i32, i32, i32)) {
        // SS_ENDELLIPSIS from WinUser.h; generated windows-sys places static
        // control styles outside this feature set.
        let style = if [SOURCE_SUMMARY, TARGET_SUMMARY].contains(&id) {
            0x00004000
        } else {
            0
        };
        self.add(id, page, "STATIC", text, style, rect, true);
    }
    unsafe fn button(&mut self, id: i32, page: i32, text: &str, rect: (i32, i32, i32, i32)) {
        self.add(
            id,
            page,
            "BUTTON",
            text,
            WS_TABSTOP | BS_OWNERDRAW as u32,
            rect,
            false,
        );
    }
    unsafe fn checkbox(&mut self, id: i32, page: i32, text: &str, y: i32) {
        self.add(
            id,
            page,
            "BUTTON",
            text,
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            (264, y, 632, 28),
            true,
        );
    }
    unsafe fn edit(
        &mut self,
        id: i32,
        page: i32,
        text: &str,
        rect: (i32, i32, i32, i32),
        style: u32,
    ) {
        self.add(
            id,
            page,
            "EDIT",
            text,
            WS_TABSTOP
                | (if style & ES_MULTILINE as u32 == 0 {
                    ES_AUTOHSCROLL as u32
                } else {
                    0
                })
                | style,
            rect,
            true,
        );
    }
    unsafe fn build(&mut self) {
        self.make_fonts();
        self.label(1, -1, "AudioPin", (24, 32, 160, 36));
        self.label(2, -1, "麦克风与降噪守护", (26, 78, 164, 22));
        for (i, text) in [
            "守护概览",
            "设备与路由",
            "应用输入偏好",
            "偏好设置",
            "关于与更新",
        ]
        .iter()
        .enumerate()
        {
            self.button(100 + i as i32, -1, text, (16, 132 + i as i32 * 48, 176, 40));
        }
        self.label(
            3,
            -1,
            concat!("v", env!("CARGO_PKG_VERSION"), "  /  Windows 原生"),
            (24, 634, 176, 24),
        );
        self.label(FOOTER, -1, "设置已保存", (240, 669, 470, 24));
        self.button(SAVE, -1, "保存并应用", (784, 658, 152, 38));

        self.label(30, 0, "守护概览", (240, 30, 680, 38));
        self.label(
            31,
            0,
            "自动检查、恢复，持续关注你的输入设备。",
            (240, 73, 680, 24),
        );
        self.label(STATUS, 0, "正在检查", (268, 124, 340, 35));
        self.label(DETAIL, 0, "正在连接音频设备…", (268, 172, 636, 48));
        self.button(PAUSE, 0, "暂停守护", (638, 122, 124, 34));
        self.button(CHECK, 0, "立即检查", (778, 122, 124, 34));
        self.label(COUNTS, 0, "", (268, 226, 636, 22));
        self.label(32, 0, "AMD 麦克风", (268, 283, 138, 24));
        self.label(SOURCE_SUMMARY, 0, "", (420, 281, 484, 26));
        self.label(AMD_SUMMARY, 0, "", (420, 311, 484, 38));
        self.label(33, 0, "Windows 输入", (268, 367, 138, 24));
        self.label(TARGET_SUMMARY, 0, "", (420, 365, 484, 26));
        self.label(ROUTE_SUMMARY, 0, "", (420, 395, 484, 38));
        self.label(10, 0, "最近活动", (240, 466, 350, 24));
        self.edit(
            EVENTS,
            0,
            "",
            (264, 506, 640, 102),
            ES_MULTILINE as u32 | ES_READONLY as u32 | WS_VSCROLL,
        );
        self.button(OPEN_LOG, 0, "完整日志", (666, 458, 110, 32));
        self.button(DIAG, 0, "导出诊断", (792, 458, 128, 32));

        self.label(11, 1, "设备与路由", (240, 30, 680, 38));
        self.label(
            12,
            1,
            "原始麦克风 → AMD 降噪 → Windows / 语音软件",
            (240, 73, 680, 24),
        );
        self.checkbox(AMD, 1, "守护 AMD 麦克风降噪", 128);
        self.label(13, 1, "AMD 使用的原始麦克风", (264, 180, 640, 26));
        self.add(
            PHYSICAL,
            1,
            "COMBOBOX",
            "",
            WS_TABSTOP | CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
            (264, 218, 640, 200),
            true,
        );
        self.label(
            34,
            1,
            "按设备标识固定。断开后等待原设备重连。",
            (264, 263, 640, 24),
        );
        self.label(
            14,
            1,
            "提供给 Windows / 语音软件的输入",
            (264, 334, 640, 26),
        );
        self.add(
            OUTPUT,
            1,
            "COMBOBOX",
            "",
            WS_TABSTOP | CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
            (264, 374, 640, 200),
            true,
        );
        self.checkbox(PIN_DEFAULT, 1, "固定 Windows 默认输入", 432);
        self.checkbox(PIN_COMMS, 1, "固定 Windows 通信输入", 474);
        self.label(
            15,
            1,
            "建议选择 AMD Streaming 作为输出。\r\n语音软件仍需选择系统默认或相应设备。",
            (264, 551, 640, 52),
        );

        self.label(16, 2, "应用输入偏好", (240, 30, 680, 38));
        self.label(
            17,
            2,
            "实验性 · 仅设置 Windows 为应用保存的首选输入",
            (240, 73, 680, 24),
        );
        self.checkbox(PIN_APPS, 2, "守护 Windows 应用输入偏好", 128);
        self.label(
            18,
            2,
            "进程文件名，每行一个（例如 YY.exe）",
            (264, 184, 640, 26),
        );
        self.edit(
            APP_NAMES,
            2,
            "",
            (264, 222, 640, 120),
            ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | WS_VSCROLL | ES_WANTRETURN as u32,
        );
        self.label(19, 2, "Windows 偏好检查结果", (264, 383, 640, 26));
        self.edit(
            APP_STATUS,
            2,
            "等待检测",
            (264, 421, 640, 88),
            ES_MULTILINE as u32 | ES_READONLY as u32 | WS_VSCROLL,
        );
        self.label(20, 2, "YY / KOOK 内部麦克风配置尚未接入。\r\n偏好一致不能证明应用实际使用了该输入。应用退出后偏好可能保留。", (264, 551, 640, 62));

        self.label(21, 3, "偏好设置", (240, 30, 680, 38));
        self.label(35, 3, "按你的使用习惯，调整后台守护。", (240, 73, 680, 24));
        self.checkbox(AUTOSTART, 3, "登录 Windows 时自动启动", 128);
        self.checkbox(MINIMIZED, 3, "自动启动时隐藏到托盘", 168);
        self.checkbox(CLOSE_TRAY, 3, "关闭窗口时保留托盘守护", 208);
        self.checkbox(RESTART_AMD, 3, "AMD 后台退出后自动尝试启动", 248);
        self.checkbox(NOTIFY, 3, "状态变化时显示 Windows 通知", 288);
        self.checkbox(
            RELOAD_AMD_PANEL,
            3,
            "恢复后重载 AMD 面板（会关闭 AMD 窗口）",
            328,
        );
        self.label(22, 3, "检查间隔 / 秒（1–60）", (264, 367, 390, 26));
        self.edit(POLL, 3, "2", (776, 362, 128, 30), ES_NUMBER as u32);
        self.label(23, 3, "修复重试上限 / 秒（5–600）", (264, 413, 390, 26));
        self.edit(RETRY, 3, "60", (776, 408, 128, 30), ES_NUMBER as u32);
        self.label(24, 3, "重复修复通知间隔 / 秒", (264, 459, 390, 26));
        self.edit(COOLDOWN, 3, "10", (776, 454, 128, 30), ES_NUMBER as u32);
        self.label(
            25,
            3,
            "配置保存在本机。手动修改 TOML 后可重新载入。",
            (264, 552, 640, 24),
        );
        self.button(OPEN_CONFIG, 3, "打开配置目录", (264, 590, 148, 34));
        self.button(RELOAD, 3, "重新载入", (428, 590, 128, 34));

        self.label(26, 4, "关于 AudioPin", (240, 30, 680, 38));
        self.label(
            36,
            4,
            "让麦克风的状态可见，让意外中断可恢复。",
            (240, 73, 680, 24),
        );
        self.label(27, 4, concat!("AudioPin ", env!("CARGO_PKG_VERSION"), " · Rust + Win32\r\nLianzy-Baimiao  /  MIT License\r\n无 WebView · 不录音 · 不上传音频"), (264, 128, 640, 80));
        self.label(28, 4, "GitHub 更新仓库", (264, 254, 640, 26));
        self.edit(REPO, 4, "", (264, 290, 640, 32), 0);
        self.checkbox(AUTH, 4, "使用本机 GitHub CLI 登录访问私有仓库", 340);
        self.checkbox(AUTO_UPDATE, 4, "启动时自动检查更新", 380);
        self.button(UPDATE, 4, "检查更新", (264, 432, 136, 34));
        self.button(RELEASES, 4, "打开发布页", (416, 432, 136, 34));
        self.button(PROJECT, 4, "项目仓库", (568, 432, 128, 34));
        self.edit(
            UPDATE_STATUS,
            4,
            "尚未检查更新",
            (264, 490, 640, 54),
            ES_MULTILINE as u32 | ES_READONLY as u32,
        );
        self.label(
            29,
            4,
            "AMD 面板状态同步尚未解决；降噪效果尚未验证。\r\n驱动版本变更后可能需要更新适配。",
            (264, 575, 640, 48),
        );
        self.apply_fonts();
    }
    unsafe fn apply_fonts(&self) {
        for (id, c) in &self.controls {
            let index = if [1, STATUS, 11, 16, 21, 26, 30].contains(id) {
                1
            } else if [
                2,
                3,
                10,
                12,
                15,
                17,
                20,
                25,
                29,
                31,
                34,
                35,
                36,
                COUNTS,
                EVENTS,
                AMD_SUMMARY,
                ROUTE_SUMMARY,
                FOOTER,
            ]
            .contains(id)
            {
                2
            } else {
                0
            };
            SendMessageW(c.hwnd, WM_SETFONT, self.fonts[index] as usize, 0);
        }
    }
    unsafe fn make_fonts(&mut self) {
        for size in [15, 26, 14] {
            self.fonts.push(CreateFontW(
                -(size as f32 * self.scale) as i32,
                0,
                0,
                0,
                if size == 26 { 600 } else { 400 },
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                DEFAULT_PITCH as u32,
                wide("Microsoft YaHei UI").as_ptr(),
            ));
        }
    }
    fn h(&self, id: i32) -> HWND {
        self.controls.get(&id).map(|c| c.hwnd).unwrap_or(null_mut())
    }
    unsafe fn set(&self, id: i32, text: &str) {
        if self.text(id) != text {
            SetWindowTextW(self.h(id), wide(text).as_ptr());
        }
    }
    unsafe fn text(&self, id: i32) -> String {
        let hwnd = self.h(id);
        let n = GetWindowTextLengthW(hwnd);
        let mut b = vec![0u16; n as usize + 1];
        let count = GetWindowTextW(hwnd, b.as_mut_ptr(), b.len() as i32);
        String::from_utf16_lossy(&b[..count as usize])
    }
    unsafe fn checked(&self, id: i32) -> bool {
        SendMessageW(self.h(id), BM_GETCHECK, 0, 0) == BST_CHECKED as isize
    }
    unsafe fn check(&self, id: i32, yes: bool) {
        SendMessageW(
            self.h(id),
            BM_SETCHECK,
            if yes { BST_CHECKED } else { BST_UNCHECKED } as usize,
            0,
        );
    }
    unsafe fn page(&mut self, page: i32) {
        self.page = page;
        SetPropW(
            self.hwnd,
            wide("AudioPin.Page").as_ptr(),
            (page + 1) as HANDLE,
        );
        for c in self.controls.values() {
            ShowWindow(
                c.hwnd,
                if c.page == -1 || c.page == page {
                    SW_SHOW
                } else {
                    SW_HIDE
                },
            );
        }
        for i in 0..5 {
            SetPropW(
                self.h(100 + i),
                wide("AudioPin.Selected").as_ptr(),
                usize::from(i == page) as HANDLE,
            );
            InvalidateRect(self.h(100 + i), null(), 1);
        }
        ShowWindow(self.h(SAVE), if page == 0 { SW_HIDE } else { SW_SHOW });
        InvalidateRect(self.hwnd, null(), 1);
    }
    unsafe fn layout(&mut self) {
        let mut r: RECT = std::mem::zeroed();
        GetClientRect(self.hwnd, &mut r);
        let extra = ((r.right as f32 / self.scale) as i32 - 968).max(0);
        let vertical = ((r.bottom as f32 / self.scale) as i32 - 719).max(0);
        for (id, c) in &self.controls {
            let x = if [SAVE, PAUSE, CHECK, POLL, RETRY, COOLDOWN, DIAG, OPEN_LOG].contains(id) {
                c.x + extra
            } else {
                c.x
            };
            let y = c.y
                + if [SAVE, FOOTER, 3].contains(id) {
                    vertical
                } else {
                    0
                };
            let width = if c.stretch && c.w > 300 {
                c.w + extra
            } else {
                c.w
            };
            MoveWindow(
                c.hwnd,
                (x as f32 * self.scale) as i32,
                (y as f32 * self.scale) as i32,
                (width as f32 * self.scale) as i32,
                (c.h as f32 * self.scale) as i32,
                1,
            );
        }
        InvalidateRect(self.hwnd, null(), 1);
    }
    unsafe fn load_fields(&mut self) {
        let c = &self.config;
        for (id, value) in [
            (AMD, c.amd_enabled),
            (PIN_DEFAULT, c.pin_default),
            (PIN_COMMS, c.pin_communications),
            (PIN_APPS, c.pin_apps),
            (NOTIFY, c.notifications),
            (AUTOSTART, c.autostart),
            (MINIMIZED, c.start_minimized),
            (CLOSE_TRAY, c.close_to_tray),
            (RESTART_AMD, c.restart_amd_if_missing),
            (RELOAD_AMD_PANEL, c.reload_amd_panel),
            (AUTH, c.use_gh_auth),
            (AUTO_UPDATE, c.auto_check_updates),
        ] {
            self.check(id, value);
        }
        for (id, value) in [
            (POLL, c.poll_seconds.to_string()),
            (RETRY, c.retry_max_seconds.to_string()),
            (COOLDOWN, c.notification_cooldown_seconds.to_string()),
            (APP_NAMES, c.apps.join("\r\n")),
            (REPO, c.update_repository.clone()),
        ] {
            self.set(id, &value);
        }
        self.set(
            PAUSE,
            if c.guard_enabled {
                "暂停守护"
            } else {
                "恢复守护"
            },
        );
        self.device_key.clear();
        self.dirty = false;
        self.set(FOOTER, "设置已保存");
    }
    unsafe fn gather(&self) -> Result<Config> {
        let mut c = self.config.clone();
        c.amd_enabled = self.checked(AMD);
        c.pin_default = self.checked(PIN_DEFAULT);
        c.pin_communications = self.checked(PIN_COMMS);
        c.pin_apps = self.checked(PIN_APPS);
        c.notifications = self.checked(NOTIFY);
        c.autostart = self.checked(AUTOSTART);
        c.start_minimized = self.checked(MINIMIZED);
        c.close_to_tray = self.checked(CLOSE_TRAY);
        c.restart_amd_if_missing = self.checked(RESTART_AMD);
        c.reload_amd_panel = self.checked(RELOAD_AMD_PANEL);
        c.use_gh_auth = self.checked(AUTH);
        c.auto_check_updates = self.checked(AUTO_UPDATE);
        c.poll_seconds = self
            .text(POLL)
            .parse()
            .map_err(|_| anyhow::anyhow!("检查间隔必须为数字"))?;
        c.retry_max_seconds = self
            .text(RETRY)
            .parse()
            .map_err(|_| anyhow::anyhow!("重试上限必须为数字"))?;
        c.notification_cooldown_seconds = self
            .text(COOLDOWN)
            .parse()
            .map_err(|_| anyhow::anyhow!("通知间隔必须为数字"))?;
        c.apps = self
            .text(APP_NAMES)
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        c.update_repository = self.text(REPO).trim().to_string();
        c.physical_input = self
            .selection(PHYSICAL, &self.physical_ids)
            .unwrap_or_else(|| c.physical_input.clone());
        c.output_input = self
            .selection(OUTPUT, &self.output_ids)
            .unwrap_or_else(|| c.output_input.clone());
        c.validate()?;
        Ok(c)
    }
    unsafe fn selection(&self, id: i32, ids: &[String]) -> Option<String> {
        let index = SendMessageW(self.h(id), CB_GETCURSEL, 0, 0);
        if index < 0 {
            None
        } else {
            ids.get(index as usize).cloned()
        }
    }
    unsafe fn save(&mut self) -> Result<()> {
        let c = self.gather()?;
        platform::autostart(c.autostart)?;
        if let Err(e) = c.save() {
            let _ = platform::autostart(self.config.autostart);
            return Err(e);
        }
        self.tx.send(Command::Apply(c.clone()))?;
        self.config = c;
        guard::log(&mut self.shared.lock().unwrap(), "配置已保存并应用");
        self.set(SAVE, "已保存");
        self.dirty = false;
        self.set(FOOTER, "设置已保存");
        Ok(())
    }
    unsafe fn refresh(&mut self) {
        let (snapshot, notification) = {
            let mut s = self.shared.lock().unwrap();
            let n = if self.config.notifications {
                s.notifications.pop_front()
            } else {
                s.notifications.clear();
                None
            };
            (s.clone(), n)
        };
        let title_changed = self.text(STATUS) != snapshot.title;
        self.set(STATUS, &snapshot.title);
        if title_changed {
            self.tray(NIM_MODIFY, None);
            InvalidateRect(self.hwnd, null(), 0);
        }
        self.set(DETAIL, &snapshot.detail);
        self.set(AMD_SUMMARY, &snapshot.amd_detail);
        self.set(ROUTE_SUMMARY, &snapshot.route_detail);
        let name = |id: &str| {
            snapshot
                .devices
                .iter()
                .find(|d| d.id == id)
                .map(|d| format!("{}{}", if d.active { "" } else { "离线 · " }, d.name))
                .unwrap_or_else(|| {
                    if id.is_empty() {
                        "尚未选择设备".into()
                    } else {
                        "已选设备未连接".into()
                    }
                })
        };
        self.set(SOURCE_SUMMARY, &name(&self.config.physical_input));
        self.set(TARGET_SUMMARY, &name(&self.config.output_input));
        self.set(
            COUNTS,
            &format!(
                "已检查 {} 次    ·    已自动恢复 {} 次    ·    检查间隔 {} 秒",
                snapshot.checks, snapshot.repaired, self.config.poll_seconds
            ),
        );
        self.set(APP_STATUS, &snapshot.apps);
        let events = snapshot
            .events
            .iter()
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\r\n");
        if self.last_events != events {
            self.set(EVENTS, &events);
            self.last_events = events;
        }
        self.set(UPDATE_STATUS, &snapshot.update);
        if self.update_busy && !snapshot.update.starts_with("正在检查") {
            self.update_busy = false;
            EnableWindow(self.h(UPDATE), 1);
        }
        if let Some(text) = notification {
            self.tray(NIM_MODIFY, Some(&text));
        }
        let key = snapshot
            .devices
            .iter()
            .map(|d| format!("{}:{}", d.id, d.active))
            .collect::<Vec<_>>()
            .join(";");
        if key != self.device_key || self.physical_ids.is_empty() {
            let selected_physical = self
                .selection(PHYSICAL, &self.physical_ids)
                .unwrap_or_else(|| self.config.physical_input.clone());
            let selected_output = self
                .selection(OUTPUT, &self.output_ids)
                .unwrap_or_else(|| self.config.output_input.clone());
            for (id, physical, selected) in [
                (PHYSICAL, true, selected_physical),
                (OUTPUT, false, selected_output),
            ] {
                SendMessageW(self.h(id), CB_RESETCONTENT, 0, 0);
                let mut ids = vec![String::new()];
                SendMessageW(
                    self.h(id),
                    CB_ADDSTRING,
                    0,
                    wide("请选择设备").as_ptr() as isize,
                );
                let mut selection = 0;
                for d in &snapshot.devices {
                    if physical && d.amd {
                        continue;
                    }
                    let label = format!("{}{}", if d.active { "" } else { "[离线] " }, d.name);
                    SendMessageW(self.h(id), CB_ADDSTRING, 0, wide(label).as_ptr() as isize);
                    if d.id == selected {
                        selection = ids.len();
                    }
                    ids.push(d.id.clone());
                }
                if !selected.is_empty() && !ids.contains(&selected) {
                    selection = ids.len();
                    ids.push(selected.clone());
                    SendMessageW(
                        self.h(id),
                        CB_ADDSTRING,
                        0,
                        wide(format!("[未连接] {selected}")).as_ptr() as isize,
                    );
                }
                SendMessageW(self.h(id), CB_SETCURSEL, selection, 0);
                if physical {
                    self.physical_ids = ids;
                } else {
                    self.output_ids = ids;
                }
            }
            self.device_key = key;
        }
    }
    unsafe fn check_update(&mut self) {
        if self.update_busy {
            return;
        }
        let mut c = self.config.clone();
        c.update_repository = self.text(REPO).trim().into();
        c.use_gh_auth = self.checked(AUTH);
        if let Err(e) = c.validate() {
            self.error(&format!("{e:#}"));
            return;
        }
        self.update_busy = true;
        EnableWindow(self.h(UPDATE), 0);
        self.shared.lock().unwrap().update = "正在检查 GitHub Release…".into();
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            let result = updates::check(&c).unwrap_or_else(|e| format!("{e:#}"));
            shared.lock().unwrap().update = result;
        });
    }
    unsafe fn error(&self, text: &str) {
        MessageBoxW(
            self.hwnd,
            wide(text).as_ptr(),
            wide("AudioPin").as_ptr(),
            MB_OK | MB_ICONWARNING,
        );
    }
    unsafe fn tray(&self, op: u32, notification: Option<&str>) -> bool {
        let mut n: NOTIFYICONDATAW = std::mem::zeroed();
        n.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        n.hWnd = self.hwnd;
        n.uID = 1;
        n.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        n.uCallbackMessage = TRAY;
        n.hIcon = LoadIconW(GetModuleHandleW(null()), resource(1));
        let title = self.shared.lock().unwrap().title.clone();
        copy_text(&mut n.szTip, &format!("AudioPin · {title}"));
        if let Some(text) = notification {
            n.uFlags |= NIF_INFO;
            copy_text(&mut n.szInfoTitle, "AudioPin 音频状态");
            copy_text(&mut n.szInfo, text);
            n.dwInfoFlags = NIIF_INFO;
        }
        Shell_NotifyIconW(op, &n) != 0
    }
    unsafe fn act(&mut self, id: i32) {
        if (100..105).contains(&id) {
            self.page(id - 100);
            return;
        }
        let result: Result<()> = (|| {
            match id {
                SAVE => self.save()?,
                PAUSE => {
                    let enabled = !self.config.guard_enabled;
                    let mut c = self.config.clone();
                    c.guard_enabled = enabled;
                    c.save()?;
                    self.config = c;
                    self.tx.send(Command::Pause(!enabled))?;
                    self.set(
                        PAUSE,
                        if enabled {
                            "暂停守护"
                        } else {
                            "恢复守护"
                        },
                    );
                    self.tray(NIM_MODIFY, None);
                }
                CHECK => {
                    self.tx.send(Command::Check)?;
                }
                OPEN_CONFIG | OPEN_LOG => {
                    std::fs::create_dir_all(data_dir())?;
                    platform::open(&data_dir().display().to_string());
                }
                RELOAD => {
                    let c = Config::load()?;
                    platform::autostart(c.autostart)?;
                    self.tx.send(Command::Apply(c.clone()))?;
                    self.config = c;
                    self.physical_ids.clear();
                    self.output_ids.clear();
                    SendMessageW(self.h(PHYSICAL), CB_RESETCONTENT, 0, 0);
                    SendMessageW(self.h(OUTPUT), CB_RESETCONTENT, 0, 0);
                    self.load_fields();
                }
                UPDATE => self.check_update(),
                RELEASES | PROJECT => {
                    let c = self.gather()?;
                    platform::open(&format!(
                        "https://github.com/{}{}",
                        c.update_repository,
                        if id == RELEASES { "/releases" } else { "" }
                    ));
                }
                DIAG => {
                    let mut child = platform::hidden_command(std::env::current_exe()?);
                    child.arg("--diagnose");
                    std::thread::spawn(move || {
                        let _ = child.status();
                    });
                    guard::log(
                        &mut self.shared.lock().unwrap(),
                        "已请求导出 diagnostics.json 到配置目录（包含本机设备 ID，仅保存在本机）",
                    );
                }
                EXIT => {
                    DestroyWindow(self.hwnd);
                }
                _ => {
                    self.dirty = true;
                    self.set(SAVE, "保存并应用");
                    self.set(FOOTER, "有未保存的更改");
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.error(&format!("{e:#}"));
        }
    }
}
fn copy_text<const N: usize>(dst: &mut [u16; N], text: &str) {
    for (a, b) in dst.iter_mut().take(N - 1).zip(text.encode_utf16()) {
        *a = b;
    }
}
unsafe fn fill(dc: HDC, rect: &RECT, color: COLORREF) {
    SetDCBrushColor(dc, color);
    FillRect(dc, rect, GetStockObject(DC_BRUSH) as HBRUSH);
}
unsafe fn rounded(dc: HDC, rect: &RECT, color: COLORREF, border: COLORREF, radius: i32) {
    let brush = CreateSolidBrush(color);
    let pen = CreatePen(PS_SOLID, 1, border);
    let old_brush = SelectObject(dc, brush);
    let old_pen = SelectObject(dc, pen);
    RoundRect(
        dc,
        rect.left,
        rect.top,
        rect.right,
        rect.bottom,
        radius,
        radius,
    );
    SelectObject(dc, old_pen);
    SelectObject(dc, old_brush);
    DeleteObject(pen);
    DeleteObject(brush);
}
unsafe fn window_text(hwnd: HWND) -> String {
    let count = GetWindowTextLengthW(hwnd);
    let mut text = vec![0u16; count as usize + 1];
    let read = GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32);
    String::from_utf16_lossy(&text[..read as usize])
}
/// Paint-only messages must be handled even while App is mutably borrowed by
/// the outer Windows callback. They never access a Rust App reference.
unsafe fn window_message(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> Option<LRESULT> {
    let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
    let px = |n: i32| (n as f32 * scale) as i32;
    match msg {
        WM_GETMINMAXINFO => {
            let m = &mut *(l as *mut MINMAXINFO);
            m.ptMinTrackSize.x = px(984);
            m.ptMinTrackSize.y = px(758);
            Some(0)
        }
        WM_DESTROY => {
            KillTimer(hwnd, 1);
            PostQuitMessage(0);
            Some(0)
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            RemovePropW(hwnd, wide("AudioPin.Page").as_ptr());
            Some(DefWindowProcW(hwnd, msg, w, l))
        }
        WM_ERASEBKGND => Some(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut r = RECT::default();
            GetClientRect(hwnd, &mut r);
            fill(dc, &r, BG);
            fill(
                dc,
                &RECT {
                    right: px(208),
                    ..r
                },
                SIDE,
            );
            let page = GetPropW(hwnd, wide("AudioPin.Page").as_ptr()) as usize;
            let cards: &[(i32, i32)] = match page {
                1 => &[(112, 256), (268, 352), (360, 444), (496, 630)],
                2 => &[(112, 306), (318, 526)],
                3 => &[(112, 530)],
                4 => &[(112, 332), (350, 516)],
                5 => &[(112, 222), (240, 554)],
                _ => &[],
            };
            for (top, bottom) in cards {
                rounded(
                    dc,
                    &RECT {
                        left: px(240),
                        top: px(*top),
                        right: r.right - px(32),
                        bottom: px(*bottom),
                    },
                    WHITE,
                    rgb(224, 231, 240),
                    px(12),
                );
            }
            if page == 1 {
                let title = window_text(GetDlgItem(hwnd, STATUS));
                let color = match title.as_str() {
                    "守护运行中" => rgb(23, 133, 94),
                    "需要关注" | "守护已停止" | "音频服务不可用" => {
                        rgb(186, 76, 37)
                    }
                    _ => ACCENT,
                };
                fill(
                    dc,
                    &RECT {
                        left: px(240),
                        right: px(244),
                        top: px(130),
                        bottom: px(154),
                    },
                    color,
                );
            }
            fill(
                dc,
                &RECT {
                    left: px(240),
                    right: r.right - px(32),
                    top: r.bottom - px(72),
                    bottom: r.bottom - px(71),
                },
                rgb(224, 231, 240),
            );
            EndPaint(hwnd, &ps);
            Some(0)
        }
        WM_DRAWITEM => {
            let draw = &*(l as *const DRAWITEMSTRUCT);
            if draw.CtlType != ODT_BUTTON {
                return None;
            }
            let id = draw.CtlID as i32;
            let nav = (100..105).contains(&id);
            let selected = !GetPropW(draw.hwndItem, wide("AudioPin.Selected").as_ptr()).is_null();
            let pressed = draw.itemState & ODS_SELECTED != 0;
            let disabled = draw.itemState & ODS_DISABLED != 0;
            let primary = id == SAVE;
            let color = if primary {
                if pressed {
                    rgb(26, 78, 178)
                } else {
                    ACCENT
                }
            } else if nav {
                if selected {
                    rgb(217, 230, 252)
                } else {
                    SIDE
                }
            } else if pressed {
                rgb(231, 238, 249)
            } else {
                WHITE
            };
            fill(
                draw.hDC,
                &draw.rcItem,
                if nav {
                    SIDE
                } else if primary || [DIAG, OPEN_LOG, OPEN_CONFIG, RELOAD].contains(&id) {
                    BG
                } else {
                    WHITE
                },
            );
            rounded(
                draw.hDC,
                &draw.rcItem,
                color,
                if nav || primary {
                    color
                } else {
                    rgb(207, 217, 231)
                },
                px(8),
            );
            if nav && selected {
                fill(
                    draw.hDC,
                    &RECT {
                        left: draw.rcItem.left + px(1),
                        right: draw.rcItem.left + px(4),
                        top: draw.rcItem.top + px(11),
                        bottom: draw.rcItem.bottom - px(11),
                    },
                    ACCENT,
                );
            }
            SetBkMode(draw.hDC, TRANSPARENT as i32);
            SetTextColor(
                draw.hDC,
                if disabled {
                    MUTED
                } else if primary {
                    WHITE
                } else if selected {
                    ACCENT
                } else {
                    INK
                },
            );
            let font = SendMessageW(draw.hwndItem, WM_GETFONT, 0, 0) as HFONT;
            let old = SelectObject(draw.hDC, font);
            let mut rect = draw.rcItem;
            if nav {
                rect.left += px(24);
            }
            let text = wide(window_text(draw.hwndItem));
            DrawTextW(
                draw.hDC,
                text.as_ptr(),
                -1,
                &mut rect,
                DT_SINGLELINE | DT_VCENTER | if nav { DT_LEFT } else { DT_CENTER },
            );
            SelectObject(draw.hDC, old);
            if draw.itemState & ODS_FOCUS != 0 && draw.itemState & ODS_NOFOCUSRECT == 0 {
                InflateRect(&mut rect, -px(4), -px(4));
                DrawFocusRect(draw.hDC, &rect);
            }
            Some(1)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT => {
            let dc = w as HDC;
            let id = GetDlgCtrlID(l as HWND);
            let background = if [1, 2, 3].contains(&id) {
                SIDE
            } else if [
                10, 11, 12, 15, 16, 17, 20, 21, 25, 26, 29, 30, 31, 35, 36, FOOTER,
            ]
            .contains(&id)
            {
                BG
            } else {
                WHITE
            };
            let muted = [
                2,
                3,
                12,
                15,
                17,
                20,
                25,
                29,
                31,
                34,
                35,
                36,
                COUNTS,
                AMD_SUMMARY,
                ROUTE_SUMMARY,
                FOOTER,
            ]
            .contains(&id);
            SetBkColor(dc, background);
            SetTextColor(dc, if muted { MUTED } else { INK });
            SetDCBrushColor(dc, background);
            Some(GetStockObject(DC_BRUSH) as isize)
        }
        _ => None,
    }
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let create = &*(l as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
    }
    // Synchronous paint/child notifications can reenter during layout, text
    // updates and modal menus. Painting uses only HWND state; never borrow App.
    if let Some(result) = window_message(hwnd, msg, w, l) {
        return result;
    }
    let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<App>;
    if pointer.is_null() {
        return DefWindowProcW(hwnd, msg, w, l);
    }
    let cell = &*pointer;
    let Ok(mut app) = cell.try_borrow_mut() else {
        // Programmatic control updates send nested notifications. Their state
        // is already being applied by the outer call and must not be replayed.
        return DefWindowProcW(hwnd, msg, w, l);
    };
    match msg {
        WM_COMMAND => {
            let code = (w >> 16) as u32;
            if [BN_CLICKED, EN_CHANGE, CBN_SELCHANGE].contains(&code) {
                app.act((w & 0xffff) as i32);
            }
            0
        }
        WM_TIMER => {
            app.refresh();
            0
        }
        WM_SIZE => {
            if w == SIZE_MINIMIZED as usize && app.tray_ok {
                ShowWindow(hwnd, SW_HIDE);
            } else {
                app.layout();
            }
            0
        }
        WM_DPICHANGED => {
            app.scale = (w & 0xffff) as f32 / 96.0;
            let r = &*(l as *const RECT);
            SetWindowPos(
                hwnd,
                null_mut(),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let old = std::mem::take(&mut app.fonts);
            app.make_fonts();
            app.apply_fonts();
            for f in old {
                DeleteObject(f);
            }
            app.layout();
            0
        }
        WM_CLOSE => {
            if app.config.close_to_tray && app.tray_ok {
                ShowWindow(hwnd, SW_HIDE);
            } else {
                DestroyWindow(hwnd);
            }
            0
        }
        SHOW => {
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
            0
        }
        TRAY => {
            match l as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                    ShowWindow(hwnd, SW_RESTORE);
                    SetForegroundWindow(hwnd);
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    let menu = CreatePopupMenu();
                    AppendMenuW(
                        menu,
                        MF_STRING,
                        SHOW as usize,
                        wide("打开 AudioPin").as_ptr(),
                    );
                    AppendMenuW(
                        menu,
                        MF_STRING,
                        PAUSE as usize,
                        wide(if app.config.guard_enabled {
                            "暂停守护"
                        } else {
                            "恢复守护"
                        })
                        .as_ptr(),
                    );
                    AppendMenuW(menu, MF_STRING, CHECK as usize, wide("立即检查").as_ptr());
                    AppendMenuW(menu, MF_SEPARATOR, 0, null());
                    AppendMenuW(
                        menu,
                        MF_STRING,
                        EXIT as usize,
                        wide("退出 AudioPin").as_ptr(),
                    );
                    let mut p: POINT = std::mem::zeroed();
                    GetCursorPos(&mut p);
                    SetForegroundWindow(hwnd);
                    let cmd = TrackPopupMenu(
                        menu,
                        TPM_RETURNCMD | TPM_RIGHTBUTTON,
                        p.x,
                        p.y,
                        0,
                        hwnd,
                        null(),
                    );
                    DestroyMenu(menu);
                    if cmd == SHOW as i32 {
                        PostMessageW(hwnd, SHOW, 0, 0);
                    } else if cmd != 0 {
                        app.act(cmd);
                    }
                    PostMessageW(hwnd, WM_NULL, 0, 0);
                }
                _ => {}
            }
            0
        }
        _ if msg == app.taskbar_message => {
            app.tray_ok = app.tray(NIM_ADD, None);
            if !app.tray_ok {
                ShowWindow(hwnd, SW_SHOW);
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
