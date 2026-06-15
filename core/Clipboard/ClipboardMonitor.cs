using System.Runtime.InteropServices;

namespace ClipX.Core.Clipboard;

public sealed class ClipboardMonitor : IDisposable
{
    private const int WM_CLIPBOARDUPDATE = 0x031D;

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool AddClipboardFormatListener(IntPtr hwnd);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool RemoveClipboardFormatListener(IntPtr hwnd);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool IsClipboardFormatAvailable(uint format);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool OpenClipboard(IntPtr hWndNewOwner);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool CloseClipboard();

    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr GetClipboardData(uint uFormat);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool EmptyClipboard();

    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr SetClipboardData(uint uFormat, IntPtr hMem);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GlobalLock(IntPtr hMem);

    [DllImport("kernel32.dll")]
    private static extern bool GlobalUnlock(IntPtr hMem);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GlobalAlloc(uint uFlags, UIntPtr dwBytes);

    [DllImport("user32.dll")]
    private static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenProcess(uint dwDesiredAccess, bool bInheritHandle, uint dwProcessId);

    [DllImport("psapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern uint GetModuleFileNameExW(IntPtr hProcess, IntPtr hModule, [Out] char[] lpFilename, uint nSize);

    [DllImport("kernel32.dll")]
    private static extern bool CloseHandle(IntPtr hObject);

    private const uint PROCESS_QUERY_LIMITED_INFORMATION = 0x1000;
    private const uint CF_UNICODETEXT = 13;
    private const uint GMEM_MOVEABLE = 0x0002;

    private readonly MessageWindow _window;
    private bool _disposed;
    private static volatile bool _suppressNext = false;

    public event Action<string, string>? ClipboardTextChanged;

    public ClipboardMonitor()
    {
        _window = new MessageWindow(OnMessage);
        if (!AddClipboardFormatListener(_window.Handle))
            throw new InvalidOperationException("Failed to add clipboard format listener.");
    }

    private void OnMessage(int msg)
    {
        if (msg != WM_CLIPBOARDUPDATE) return;
        if (_suppressNext) { _suppressNext = false; return; }
        if (!IsClipboardFormatAvailable(CF_UNICODETEXT)) return;

        var sourceProcess = GetForegroundProcessName();
        var text = GetClipboardText();
        if (text != null)
            ClipboardTextChanged?.Invoke(text, sourceProcess);
    }

    public static string GetForegroundProcessName()
    {
        try
        {
            var hwnd = GetForegroundWindow();
            if (hwnd == IntPtr.Zero) return "";
            GetWindowThreadProcessId(hwnd, out uint pid);
            if (pid == 0) return "";
            var hProcess = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
            if (hProcess == IntPtr.Zero) return "";
            try
            {
                var buffer = new char[1024];
                var len = GetModuleFileNameExW(hProcess, IntPtr.Zero, buffer, (uint)buffer.Length);
                if (len == 0) return "";
                var fullPath = new string(buffer, 0, (int)len);
                return System.IO.Path.GetFileName(fullPath).ToLowerInvariant();
            }
            finally
            {
                CloseHandle(hProcess);
            }
        }
        catch
        {
            return "";
        }
    }

    public static string? GetClipboardText()
    {
        if (!OpenClipboard(IntPtr.Zero)) return null;
        try
        {
            if (!IsClipboardFormatAvailable(CF_UNICODETEXT)) return null;
            var hData = GetClipboardData(CF_UNICODETEXT);
            if (hData == IntPtr.Zero) return null;
            var ptr = GlobalLock(hData);
            if (ptr == IntPtr.Zero) return null;
            try
            {
                return Marshal.PtrToStringUni(ptr);
            }
            finally
            {
                GlobalUnlock(hData);
            }
        }
        finally
        {
            CloseClipboard();
        }
    }

    public static bool SetClipboardText(string text)
    {
        _suppressNext = true;
        if (!OpenClipboard(IntPtr.Zero)) { _suppressNext = false; return false; }
        try
        {
            EmptyClipboard();
            var bytes = (text.Length + 1) * 2;
            var hGlobal = GlobalAlloc(GMEM_MOVEABLE, (UIntPtr)bytes);
            if (hGlobal == IntPtr.Zero) return false;
            var ptr = GlobalLock(hGlobal);
            if (ptr == IntPtr.Zero) return false;
            try
            {
                Marshal.Copy(text.ToCharArray(), 0, ptr, text.Length);
                Marshal.WriteInt16(ptr, text.Length * 2, 0);
            }
            finally
            {
                GlobalUnlock(hGlobal);
            }
            return SetClipboardData(CF_UNICODETEXT, hGlobal) != IntPtr.Zero;
        }
        finally
        {
            CloseClipboard();
        }
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        RemoveClipboardFormatListener(_window.Handle);
        _window.Dispose();
    }
}

internal sealed class MessageWindow : IDisposable
{
    private const string ClassName = "ClipXMessageWindow";

    [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern ushort RegisterClassW(ref WNDCLASS lpWndClass);

    [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern IntPtr CreateWindowExW(
        uint dwExStyle, string lpClassName, string lpWindowName,
        uint dwStyle, int x, int y, int nWidth, int nHeight,
        IntPtr hWndParent, IntPtr hMenu, IntPtr hInstance, IntPtr lpParam);

    [DllImport("user32.dll")]
    private static extern bool DestroyWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern IntPtr DefWindowProcW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern bool GetMessageW(out MSG lpMsg, IntPtr hWnd, uint wMsgFilterMin, uint wMsgFilterMax);

    [DllImport("user32.dll")]
    private static extern bool TranslateMessage(ref MSG lpMsg);

    [DllImport("user32.dll")]
    private static extern IntPtr DispatchMessageW(ref MSG lpMsg);

    [DllImport("user32.dll")]
    private static extern bool PostMessageW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GetModuleHandleW(string? lpModuleName);

    private delegate IntPtr WndProc(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct WNDCLASS
    {
        public uint style;
        public WndProc lpfnWndProc;
        public int cbClsExtra;
        public int cbWndExtra;
        public IntPtr hInstance;
        public IntPtr hIcon;
        public IntPtr hCursor;
        public IntPtr hbrBackground;
        public string? lpszMenuName;
        public string lpszClassName;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MSG
    {
        public IntPtr hwnd;
        public uint message;
        public IntPtr wParam;
        public IntPtr lParam;
        public uint time;
        public int pt_x;
        public int pt_y;
    }

    private const uint WM_QUIT = 0x0012;

    public IntPtr Handle { get; }
    private readonly Action<int> _onMessage;
    private readonly Thread _thread;
    private readonly WndProc _wndProcDelegate;

    public MessageWindow(Action<int> onMessage)
    {
        _onMessage = onMessage;
        _wndProcDelegate = WndProcHandler;

        var ready = new ManualResetEventSlim(false);
        IntPtr hwnd = IntPtr.Zero;

        _thread = new Thread(() =>
        {
            var hInstance = GetModuleHandleW(null);
            var wc = new WNDCLASS
            {
                lpfnWndProc = _wndProcDelegate,
                hInstance = hInstance,
                lpszClassName = ClassName + Guid.NewGuid().ToString("N")
            };
            RegisterClassW(ref wc);

            hwnd = CreateWindowExW(0, wc.lpszClassName, "ClipX", 0, 0, 0, 0, 0,
                IntPtr.Zero, IntPtr.Zero, hInstance, IntPtr.Zero);
            ready.Set();

            while (GetMessageW(out var msg, IntPtr.Zero, 0, 0))
            {
                TranslateMessage(ref msg);
                DispatchMessageW(ref msg);
            }
        });
        _thread.SetApartmentState(ApartmentState.STA);
        _thread.IsBackground = true;
        _thread.Start();
        ready.Wait();
        Handle = hwnd;
    }

    private IntPtr WndProcHandler(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam)
    {
        _onMessage((int)msg);
        return DefWindowProcW(hWnd, msg, wParam, lParam);
    }

    public void Dispose()
    {
        PostMessageW(Handle, WM_QUIT, IntPtr.Zero, IntPtr.Zero);
        _thread.Join(2000);
        DestroyWindow(Handle);
    }
}
