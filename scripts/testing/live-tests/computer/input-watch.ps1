# Who pressed what: a read-only low-level keyboard and mouse hook that logs every
# key press and button press with whether Windows marks it injected (sent by a
# program through SendInput, as the agent's tool and autopilot do) or not (a
# person at the keyboard or mouse). One JSON line per event:
#   {"t": ms, "kind": "key"|"button", "code": vk, "injected": true|false}
# Mouse moves are not logged. It runs until <Out>.stop exists or -Seconds pass.
#
#   input-watch.ps1 -Out <file.jsonl> [-Seconds 5400]
param([Parameter(Mandatory)][string]$Out, [double]$Seconds = 5400)

Add-Type -ReferencedAssemblies System.Windows.Forms -TypeDefinition @'
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Windows.Forms;

public static class InputWatch {
    delegate IntPtr Hook(int code, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] static extern IntPtr SetWindowsHookEx(int id, Hook hook, IntPtr module, uint thread);
    [DllImport("user32.dll")] static extern bool UnhookWindowsHookEx(IntPtr hook);
    [DllImport("user32.dll")] static extern IntPtr CallNextHookEx(IntPtr hook, int code, IntPtr wParam, IntPtr lParam);
    [DllImport("kernel32.dll")] static extern IntPtr GetModuleHandle(string name);

    static Hook keyboard = OnKey, mouse = OnMouse;
    static IntPtr keyboardHook, mouseHook;
    static StreamWriter log;

    static long Now() { return DateTimeOffset.Now.ToUnixTimeMilliseconds(); }

    static void Write(string kind, int code, bool injected) {
        log.WriteLine("{\"t\":" + Now() + ",\"kind\":\"" + kind + "\",\"code\":" + code + ",\"injected\":" + (injected ? "true" : "false") + "}");
        log.Flush();
    }

    // WM_KEYDOWN 0x100, WM_SYSKEYDOWN 0x104; KBDLLHOOKSTRUCT: vk at 0, flags at 8 (LLKHF_INJECTED 0x10).
    static IntPtr OnKey(int code, IntPtr wParam, IntPtr lParam) {
        int message = wParam.ToInt32();
        if (code >= 0 && (message == 0x100 || message == 0x104)) {
            Write("key", Marshal.ReadInt32(lParam, 0), (Marshal.ReadInt32(lParam, 8) & 0x10) != 0);
        }
        return CallNextHookEx(keyboardHook, code, wParam, lParam);
    }

    // Button downs (left 0x201, right 0x204, middle 0x207); MSLLHOOKSTRUCT flags at 12 (LLMHF_INJECTED 1).
    static IntPtr OnMouse(int code, IntPtr wParam, IntPtr lParam) {
        int message = wParam.ToInt32();
        if (code >= 0 && (message == 0x201 || message == 0x204 || message == 0x207)) {
            Write("button", message, (Marshal.ReadInt32(lParam, 12) & 1) != 0);
        }
        return CallNextHookEx(mouseHook, code, wParam, lParam);
    }

    public static void Run(string path, double seconds) {
        log = new StreamWriter(path, true, new UTF8Encoding(false));
        IntPtr module = GetModuleHandle(null);
        keyboardHook = SetWindowsHookEx(13, keyboard, module, 0);
        mouseHook = SetWindowsHookEx(14, mouse, module, 0);
        DateTime end = DateTime.Now.AddSeconds(seconds);
        Timer timer = new Timer();
        timer.Interval = 500;
        timer.Tick += delegate {
            if (DateTime.Now > end || File.Exists(path + ".stop")) { Application.ExitThread(); }
        };
        timer.Start();
        Application.Run();
        UnhookWindowsHookEx(keyboardHook);
        UnhookWindowsHookEx(mouseHook);
        log.Close();
    }
}
'@
[InputWatch]::Run($Out, $Seconds)
