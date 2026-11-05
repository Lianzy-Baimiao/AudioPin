use crate::{
    config::{data_dir, Config},
    guard::{self, Command, Shared},
    platform::{self, wide},
    updates,
};
use anyhow::{bail, Result};
use std::{
    collections::HashMap,
    ptr::{null, null_mut},
    sync::mpsc::Sender,
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
    tx: Sender<Command>,
    page: i32,
    scale: f32,
    physical_ids: Vec<String>,
    output_ids: Vec<String>,
    device_key: String,
    tray_ok: bool,
    taskbar_message: u32,
    update_busy: bool,
    last_events: String,
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
        let mut app = Box::new(App {
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
        });
        let scale = GetDpiForSystem() as f32 / 96.0;
        let hwnd = CreateWindowExW(
            0,
            wide(CLASS).as_ptr(),
            wide("AudioPin · 音频守护").as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            (960.0 * scale) as i32,
            (720.0 * scale) as i32,
            null_mut(),
            null_mut(),
            module,
            (&mut *app as *mut App).cast(),
        );
        if hwnd.is_null() {
            let _ = app.tx.send(Command::Stop);
            let _ = worker.join();
            bail!("创建窗口失败");
        }
        app.hwnd = hwnd;
        app.scale = GetDpiForWindow(hwnd) as f32 / 96.0;
        app.build();
        app.tray_ok = app.tray(NIM_ADD, None);
        app.load_fields();
        app.page(0);
        app.layout();
        SetTimer(hwnd, 1, 500, None);
        if !background || !app.config.start_minimized || !app.tray_ok {
            ShowWindow(hwnd, SW_SHOW);
            ShowWindow(hwnd, SW_SHOW);
        }
        if app.config.auto_check_updates {
            app.check_update();
        }
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        app.tray(NIM_DELETE, None);
        let _ = app.tx.send(Command::Stop);
        let _ = worker.join();
        for f in &app.fonts {
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
            if class == "EDIT" || class == "COMBOBOX" {
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
        self.add(id, page, "STATIC", text, 0, rect, true);
    }
    unsafe fn button(&mut self, id: i32, page: i32, text: &str, rect: (i32, i32, i32, i32)) {
        self.add(
            id,
            page,
            "BUTTON",
            text,
            WS_TABSTOP | BS_PUSHBUTTON as u32,
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
            (210, y, 660, 28),
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
        self.label(1, -1, "AudioPin", (26, 26, 150, 40));
        SendMessageW(self.h(1), WM_SETFONT, self.fonts[1] as usize, 1);
        self.label(2, -1, "让声音一直在线", (27, 70, 150, 28));
        for (i, text) in [
            "守护概览",
            "设备与路由",
            "语音软件",
            "偏好设置",
            "关于与更新",
        ]
        .iter()
        .enumerate()
        {
            self.button(100 + i as i32, -1, text, (20, 124 + i as i32 * 49, 158, 38));
        }
        self.label(
            3,
            -1,
            concat!("v", env!("CARGO_PKG_VERSION"), "\r\nRust · Windows 原生"),
            (26, 548, 155, 52),
        );
        self.button(SAVE, -1, "保存并应用", (742, 612, 150, 36));
        self.button(PAUSE, 0, "暂停守护", (210, 176, 130, 34));
        self.button(CHECK, 0, "立即检查", (352, 176, 130, 34));
        self.label(STATUS, 0, "正在检查", (210, 31, 680, 42));
        SendMessageW(self.h(STATUS), WM_SETFONT, self.fonts[1] as usize, 1);
        self.label(
            DETAIL,
            0,
            "连接 AMD 后台与 Windows 音频服务…",
            (210, 84, 670, 76),
        );
        self.label(COUNTS, 0, "", (210, 230, 670, 30));
        self.label(10, 0, "最近事件", (210, 282, 660, 30));
        self.edit(
            EVENTS,
            0,
            "",
            (210, 317, 680, 215),
            ES_MULTILINE as u32 | ES_READONLY as u32 | WS_VSCROLL | ES_AUTOVSCROLL as u32,
        );
        self.button(OPEN_LOG, 0, "打开日志目录", (210, 548, 140, 34));
        self.button(DIAG, 0, "导出本机诊断", (365, 548, 150, 34));

        self.label(11, 1, "设备与路由", (210, 31, 680, 40));
        self.label(12,1,"原始麦克风 → AMD 降噪 → 语音软件\r\n按设备 ID 固定；原设备离线时等待重连，不自动改用远程虚拟麦克风。",(210,82,680,60));
        self.checkbox(AMD, 1, "守护 AMD 麦克风降噪", 151);
        self.label(13, 1, "AMD 使用的原始麦克风", (210, 199, 680, 27));
        self.add(
            PHYSICAL,
            1,
            "COMBOBOX",
            "",
            WS_TABSTOP | CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
            (210, 230, 680, 210),
            true,
        );
        self.label(
            14,
            1,
            "提供给 Windows / 语音软件的输入（建议选 AMD Streaming）",
            (210, 294, 680, 27),
        );
        self.add(
            OUTPUT,
            1,
            "COMBOBOX",
            "",
            WS_TABSTOP | CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
            (210, 326, 680, 210),
            true,
        );
        self.checkbox(
            PIN_DEFAULT,
            1,
            "固定 Windows 默认输入（控制台 / 多媒体）",
            391,
        );
        self.checkbox(PIN_COMMS, 1, "固定 Windows 默认通信输入", 431);
        self.label(15,1,"提示：先确认 AMD 选中正确的原始麦克风。不要将降噪输出当作原始输入。\r\n暂停守护时，会尽可能还原本次运行修改过的系统与应用路由。",(210,493,680,70));

        self.label(16, 2, "语音软件", (210, 31, 680, 40));
        self.label(17,2,"固定指定进程的 Windows 音频输入路由。应用启动后自动应用。\r\n在 YY / KOOK 内选择“系统默认”或“默认通信设备”，必要时重进语音频道。",(210,82,680,65));
        self.checkbox(PIN_APPS, 2, "启用应用输入守护", 158);
        self.label(
            18,
            2,
            "进程文件名（每行一个；可在任务管理器“详细信息”中查看）",
            (210, 208, 680, 28),
        );
        self.edit(
            APP_NAMES,
            2,
            "",
            (210, 241, 680, 130),
            ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | WS_VSCROLL | ES_WANTRETURN as u32,
        );
        self.label(19, 2, "当前应用状态", (210, 399, 680, 28));
        self.edit(
            APP_STATUS,
            2,
            "等待检测",
            (210, 434, 680, 106),
            ES_MULTILINE as u32 | ES_READONLY as u32 | WS_VSCROLL,
        );
        self.label(
            20,
            2,
            "软件内部显式选定的麦克风可能优先于 Windows 路由；AudioPin 不修改聊天数据。",
            (210, 557, 680, 40),
        );

        self.label(21, 3, "偏好设置", (210, 31, 680, 40));
        self.checkbox(AUTOSTART, 3, "登录 Windows 时自动启动", 88);
        self.checkbox(MINIMIZED, 3, "自动启动时隐藏到托盘", 126);
        self.checkbox(CLOSE_TRAY, 3, "关闭窗口时保留托盘守护", 164);
        self.checkbox(RESTART_AMD, 3, "AMD 后台意外退出时尝试重新启动", 202);
        self.checkbox(NOTIFY, 3, "状态变化时显示 Windows 托盘通知", 240);
        self.label(22, 3, "检查间隔 / 秒（1–60）", (210, 301, 260, 28));
        self.edit(POLL, 3, "2", (505, 296, 95, 30), ES_NUMBER as u32);
        self.label(23, 3, "失败重试上限 / 秒（5–600）", (210, 344, 270, 28));
        self.edit(RETRY, 3, "60", (505, 339, 95, 30), ES_NUMBER as u32);
        self.label(24, 3, "重复修复通知间隔 / 秒", (210, 387, 270, 28));
        self.edit(COOLDOWN, 3, "10", (505, 382, 95, 30), ES_NUMBER as u32);
        self.label(25,3,"配置存放在 %LOCALAPPDATA%\\AudioPin。\r\n编辑 TOML 后点击重新载入；语法错误会保留当前正在使用的配置。",(210,445,680,64));
        self.button(OPEN_CONFIG, 3, "打开配置目录", (210, 525, 150, 34));
        self.button(RELOAD, 3, "重新载入配置", (375, 525, 150, 34));

        self.label(26, 4, "关于 AudioPin", (210, 31, 680, 40));
        self.label(27,4,concat!("版本 ",env!("CARGO_PKG_VERSION")," · Windows x64\r\n作者：Lianzy-Baimiao  |  MIT License\r\nRust + Win32 原生控件，无 WebView；不录音、不上传音频、不采集遥测。"),(210,82,680,91));
        self.label(28, 4, "GitHub 更新仓库", (210, 194, 680, 28));
        self.edit(REPO, 4, "", (210, 229, 680, 32), 0);
        self.checkbox(AUTH, 4, "使用本机 GitHub CLI 登录访问私有仓库", 284);
        self.checkbox(AUTO_UPDATE, 4, "每次启动自动检查更新（需要网络）", 324);
        self.button(UPDATE, 4, "检查 GitHub 更新", (210, 377, 175, 35));
        self.button(RELEASES, 4, "打开发布页", (398, 377, 135, 35));
        self.button(PROJECT, 4, "项目仓库", (546, 377, 135, 35));
        self.edit(
            UPDATE_STATUS,
            4,
            "尚未检查更新",
            (210, 431, 680, 92),
            ES_MULTILINE as u32 | ES_READONLY as u32 | WS_VSCROLL,
        );
        self.label(29,4,"私有仓库需先执行 gh auth login。令牌由 GitHub CLI 保管，不写入配置。\r\n更新检查只读取 Release；下载与安装由你从发布页操作。\r\nAMD 控制适配经过版本校验；驱动变更后可能需要更新 AudioPin。",(210,540,680,66));
        for id in [11, 16, 21, 26] {
            SendMessageW(self.h(id), WM_SETFONT, self.fonts[1] as usize, 1);
        }
    }
    unsafe fn make_fonts(&mut self) {
        for size in [16, 27] {
            self.fonts.push(CreateFontW(
                -(size as f32 * self.scale) as i32,
                0,
                0,
                0,
                if size == 27 { 600 } else { 400 },
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
            SendMessageW(self.h(100 + i), BM_SETSTATE, usize::from(i == page), 0);
        }
    }
    unsafe fn layout(&mut self) {
        let mut r: RECT = std::mem::zeroed();
        GetClientRect(self.hwnd, &mut r);
        let extra = ((r.right as f32 / self.scale) as i32 - 924).max(0);
        for (id, c) in &self.controls {
            let x = if *id == SAVE { c.x + extra } else { c.x };
            let width = if c.stretch && c.w > 300 {
                c.w + extra
            } else {
                c.w
            };
            MoveWindow(
                c.hwnd,
                (x as f32 * self.scale) as i32,
                (c.y as f32 * self.scale) as i32,
                (width as f32 * self.scale) as i32,
                (c.h as f32 * self.scale) as i32,
                1,
            );
        }
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
        Ok(())
    }
    unsafe fn refresh(&mut self) {
        let (snapshot, notification) = {
            let mut s = self.shared.lock().unwrap();
            let n = s.pending_notification.take();
            (s.clone(), n)
        };
        let title_changed = self.text(STATUS) != snapshot.title;
        self.set(STATUS, &snapshot.title);
        if title_changed {
            self.tray(NIM_MODIFY, None);
        }
        self.set(DETAIL, &snapshot.detail);
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
        if key != self.device_key && !snapshot.devices.is_empty() {
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
                    self.set(SAVE, "保存并应用");
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
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let create = &*(l as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
    }
    let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    if pointer.is_null() {
        return DefWindowProcW(hwnd, msg, w, l);
    }
    let app = &mut *pointer;
    match msg {
        WM_COMMAND => {
            app.act((w & 0xffff) as i32);
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
        WM_GETMINMAXINFO => {
            let m = &mut *(l as *mut MINMAXINFO);
            let scale = if app.scale > 0.0 { app.scale } else { 1.0 };
            m.ptMinTrackSize.x = (940.0 * scale) as i32;
            m.ptMinTrackSize.y = (710.0 * scale) as i32;
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
            for (id, c) in &app.controls {
                SendMessageW(
                    c.hwnd,
                    WM_SETFONT,
                    app.fonts[usize::from([1, STATUS, 11, 16, 21, 26].contains(id))] as usize,
                    1,
                );
            }
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
        WM_DESTROY => {
            KillTimer(hwnd, 1);
            PostQuitMessage(0);
            0
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let dc = w as HDC;
            SetBkColor(dc, GetSysColor(COLOR_WINDOW));
            SetTextColor(dc, GetSysColor(COLOR_WINDOWTEXT));
            GetSysColorBrush(COLOR_WINDOW) as isize
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
