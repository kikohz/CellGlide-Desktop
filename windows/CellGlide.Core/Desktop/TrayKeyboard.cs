namespace CellGlide.Core.Desktop;

/// <summary>The tray icon's notifications that may come from the keyboard (H.NotifyIcon's KeyboardEvent).</summary>
public enum TrayKey
{
    /// <summary>NIN_SELECT (0x400): also sent after a left click.</summary>
    Select,

    /// <summary>NIN_KEYSELECT (0x401): Enter or Space on the focused icon.</summary>
    KeySelect,

    /// <summary>WM_CONTEXTMENU (0x007B): Shift+F10 or the menu key; also sent after a right click.</summary>
    ContextMenu,
}

/// <summary>
/// Which of the tray icon's keyboard notifications open (or close) the panel (decision 12). The shell
/// sends NIN_SELECT after a left click's button-up, and WM_CONTEXTMENU between a right click's button-down
/// and button-up (seen on Windows 11 23H2). Windows 11 also turns Enter and Space on the focused icon into a
/// left click (so the click opens the panel) and Shift+F10 into a right click (so the icon's menu opens, as
/// for the mouse). Those clicks are handled as clicks: a Select or ContextMenu within <see cref="Window"/> of
/// a mouse button going down or up is not a key. What is left (NIN_KEYSELECT, and NIN_SELECT or
/// WM_CONTEXTMENU with no click, from a shell that does not make clicks of keys) is a key. Enter may send
/// its notification twice: a second one within <see cref="Window"/> of the last key is the same press.
/// </summary>
public sealed class TrayKeyboard
{
    /// <summary>How long after a click, or after a key, a notification belongs to it.</summary>
    public static readonly TimeSpan Window = TimeSpan.FromMilliseconds(500);

    TimeSpan? _mouseAt;
    TimeSpan? _keyAt;

    /// <summary>A mouse button went down or up on the icon at <paramref name="now"/> (a real click or one the shell made of a key).</summary>
    public void Mouse(TimeSpan now) => _mouseAt = now;

    /// <summary>Whether the notification <paramref name="key"/> at <paramref name="now"/> toggles the panel.</summary>
    public bool Toggles(TrayKey key, TimeSpan now)
    {
        if (key != TrayKey.KeySelect && _mouseAt is { } click && now - click < Window)
        {
            return false;
        }
        if (_keyAt is { } last && now - last < Window)
        {
            return false;
        }
        _keyAt = now;
        return true;
    }
}
