using CellGlide.Core.Models;

namespace CellGlide.Core.Desktop;

/// <summary>When the tray's native menu needs building again.</summary>
public static class TrayMenu
{
    /// <summary>
    /// Whether two menus (<see cref="ChromeView.Menu"/>) show the same items: same kinds, labels and
    /// enabled states, in the same order. Every state event brings a new list; only a different one
    /// is worth rebuilding the menu for.
    /// </summary>
    public static bool Same(IReadOnlyList<MenuItemView>? shown, IReadOnlyList<MenuItemView>? next) =>
        shown is not null && next is not null && shown.SequenceEqual(next);
}
