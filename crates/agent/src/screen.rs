//! Screenshots on Windows.
//!
//! The service runs in session 0, which has no view of anyone's desktop. So the agent
//! (as LocalSystem) finds the signed-in person's session, starts `yonder-agent
//! capture-screen -` there with their token, and reads the PNG it writes to stdout; that
//! helper copies every monitor. A pipe, not a file: the service's temp folder
//! (`C:\Windows\SystemTemp`) is closed to the signed-in person. Only devices with the
//! Admin permission can ask for one.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use std::ptr::{null, null_mut};

use anyhow::{Context, Result, bail};
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
    CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC,
    SRCCOPY, SelectObject,
};
use windows_sys::Win32::System::RemoteDesktop::{
    WTS_CURRENT_SERVER_HANDLE, WTS_SESSION_INFOW, WTSActive, WTSEnumerateSessionsW, WTSFreeMemory,
    WTSQueryUserToken,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessAsUserW, GetExitCodeProcess, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// A handle closed when dropped.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a handle this process opened and nothing else closes.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text).encode_wide().chain([0]).collect()
}

/// A pipe whose write end the child inherits: (read end, write end).
fn pipe() -> Result<(Owned, Owned)> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let (mut read, mut write): (HANDLE, HANDLE) = (null_mut(), null_mut());
    // SAFETY: out-pointers to locals.
    if unsafe { CreatePipe(&mut read, &mut write, &attributes, 0) } == 0 {
        return Err(std::io::Error::last_os_error()).context("couldn't make a pipe");
    }
    let (read, write) = (Owned(read), Owned(write));
    // SAFETY: a handle just made; only the write end goes to the child.
    unsafe { SetHandleInformation(read.0, HANDLE_FLAG_INHERIT, 0) };
    Ok((read, write))
}

/// Reads a pipe to its end on a thread.
fn drain(read: Owned) -> std::thread::JoinHandle<Vec<u8>> {
    // SAFETY: the File takes over the handle; `forget` keeps Owned from closing it too.
    let mut file = unsafe { File::from_raw_handle(read.0 as _) };
    std::mem::forget(read);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = file.read_to_end(&mut bytes);
        bytes
    })
}

/// The session someone is signed in to and using, if any.
fn active_session() -> Option<u32> {
    let mut sessions: *mut WTS_SESSION_INFOW = null_mut();
    let mut count = 0u32;
    // SAFETY: WTS allocates the list; it's freed below.
    let ok = unsafe {
        WTSEnumerateSessionsW(WTS_CURRENT_SERVER_HANDLE, 0, 1, &mut sessions, &mut count)
    };
    if ok == 0 || sessions.is_null() {
        return None;
    }
    // SAFETY: `count` entries, valid until WTSFreeMemory.
    let list = unsafe { std::slice::from_raw_parts(sessions, count as usize) };
    let found = list
        .iter()
        .find(|s| s.State == WTSActive && s.SessionId != 0)
        .map(|s| s.SessionId);
    // SAFETY: allocated by WTSEnumerateSessionsW.
    unsafe { WTSFreeMemory(sessions.cast()) };
    found
}

/// As the service: takes a screenshot into `out` by running the capture helper in the
/// signed-in person's session.
pub fn in_user_session(out: &Path) -> Result<()> {
    let Some(session) = active_session() else {
        bail!(
            "nobody is signed in at the machine, so there's no screen to capture; \
             use Remote Desktop or Parsec to sign in"
        );
    };
    let mut token: HANDLE = null_mut();
    // SAFETY: plain call; LocalSystem may query session tokens.
    if unsafe { WTSQueryUserToken(session, &mut token) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("couldn't act in the signed-in session (the agent must run as the service)");
    }
    let token = Owned(token);

    let program = std::env::current_exe()?;
    let mut line = crate::build_user::command_line(&[
        program.as_os_str(),
        "capture-screen".as_ref(),
        "-".as_ref(),
    ]);
    let (out_read, out_write) = pipe()?;
    let (err_read, err_write) = pipe()?;
    let mut desktop = wide(r"winsta0\default");
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        lpDesktop: desktop.as_mut_ptr(),
        dwFlags: STARTF_USESTDHANDLES,
        hStdOutput: out_write.0,
        hStdError: err_write.0,
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    // SAFETY: every buffer outlives the call; the command line is mutable as required.
    let ok = unsafe {
        CreateProcessAsUserW(
            token.0,
            null(),
            line.as_mut_ptr(),
            null(),
            null(),
            1,
            CREATE_NO_WINDOW,
            null(),
            null(),
            &startup,
            &mut process,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error())
            .context("couldn't start the capture in the signed-in session");
    }
    let (child, _thread) = (Owned(process.hProcess), Owned(process.hThread));
    // The child holds its own copies; ours would keep the pipes open forever.
    drop((out_write, err_write));
    let (png, errors) = (drain(out_read), drain(err_read));
    // SAFETY: a valid process handle.
    let waited = unsafe { WaitForSingleObject(child.0, 20_000) };
    if waited != WAIT_OBJECT_0 {
        // SAFETY: a valid process handle.
        unsafe { TerminateProcess(child.0, 1) };
    }
    let png = png.join().unwrap_or_default();
    let errors = errors.join().unwrap_or_default();
    if waited != WAIT_OBJECT_0 {
        bail!("the screen capture didn't finish in time");
    }
    let mut code = 1u32;
    // SAFETY: a valid process handle.
    unsafe { GetExitCodeProcess(child.0, &mut code) };
    if code != 0 || png.is_empty() {
        let why = String::from_utf8_lossy(&errors);
        let why = why.trim().trim_start_matches("Error:").trim();
        if why.is_empty() {
            bail!("the screen capture failed (exit {code}); is the screen locked or asleep?");
        }
        bail!("the screen capture failed: {why}");
    }
    std::fs::write(out, png).with_context(|| format!("couldn't write {}", out.display()))
}

/// In the signed-in session: copies every monitor into a PNG at `out` (`-`: stdout).
pub fn capture(out: &Path) -> Result<()> {
    // Full-resolution pixels on scaled (HiDPI) screens.
    // SAFETY: plain call, before any window.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    // SAFETY: plain calls.
    let (x, y, width, height) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    if width <= 0 || height <= 0 {
        bail!("no screen to capture");
    }
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    // SAFETY: GDI objects created here are released before returning; the buffer holds
    // width * height 32-bit pixels, as the bitmap header asks for.
    let copied = unsafe {
        let screen = GetDC(null_mut());
        let memory = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        let previous = SelectObject(memory, bitmap);
        let blit = BitBlt(
            memory,
            0,
            0,
            width,
            height,
            screen,
            x,
            y,
            SRCCOPY | CAPTUREBLT,
        );
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative: rows top to bottom.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let lines = GetDIBits(
            memory,
            bitmap,
            0,
            height as u32,
            pixels.as_mut_ptr().cast(),
            &mut info,
            DIB_RGB_COLORS,
        );
        SelectObject(memory, previous);
        DeleteObject(bitmap);
        DeleteDC(memory);
        ReleaseDC(null_mut(), screen);
        blit != 0 && lines == height
    };
    if !copied {
        bail!("couldn't copy the screen");
    }
    // GDI gives blue, green, red, unused; PNG wants red, green, blue, alpha.
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    let mut encoded = Vec::new();
    let mut encoder = png::Encoder::new(&mut encoded, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&pixels)?;
    writer.finish()?;
    if out == Path::new("-") {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&encoded)?;
        stdout.flush()?;
        return Ok(());
    }
    std::fs::write(out, encoded).with_context(|| format!("couldn't write {}", out.display()))
}
