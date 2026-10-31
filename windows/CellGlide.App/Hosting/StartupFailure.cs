using System.Runtime.InteropServices;

namespace CellGlide.App.Hosting;

/// <summary>
/// The one message shown when cellglide_native.dll cannot be started (missing or refusing).
/// </summary>
/// <remarks>
/// These are the only user-visible strings in the app that do not come from the DLL: every other
/// text is cg-i18n's, fetched through the DLL, which is exactly what is unavailable here. So the
/// text is fixed, in both languages (English first, then Chinese), and lives only in this file.
/// </remarks>
static partial class StartupFailure
{
    /// <summary>The process exit code after showing it.</summary>
    public const int ExitCode = 2;

    const string Caption = "CellGlide";

    const string Text =
        "The CellGlide installation is incomplete. Please reinstall CellGlide.\n\n" +
        "CellGlide 安装不完整，请重新安装 CellGlide。";

    const uint MbOk = 0x0;
    const uint MbIconError = 0x10;
    const uint MbSetForeground = 0x10000;

    /// <summary>Shows the message and waits for OK.</summary>
    public static void Show() => _ = MessageBoxW(0, Text, Caption, MbOk | MbIconError | MbSetForeground);

    [LibraryImport("user32.dll", StringMarshalling = StringMarshalling.Utf16)]
    private static partial int MessageBoxW(nint owner, string text, string caption, uint type);
}
