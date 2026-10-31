using CellGlide.Core.Models;

namespace CellGlide.Core.Desktop;

/// <summary>Which of the DLL's dots goes beside a sidebar section.</summary>
public static class SidebarBadges
{
    /// <summary>
    /// The section's dot from <see cref="ChromeView.Badges"/>: Overview, History and Settings have
    /// one field each; the other sections never have a dot. This only picks the field; whether it is
    /// a warning is the DLL's.
    /// </summary>
    public static Badge For(BadgesView badges, Section section) => section switch
    {
        Section.Overview => badges.Overview,
        Section.History => badges.History,
        Section.Settings => badges.Settings,
        _ => Badge.None,
    };
}

/// <summary>When the main window's NavigationView needs its items built again.</summary>
public static class SidebarLayout
{
    /// <summary>
    /// Whether two sidebars (<see cref="ChromeView.Sidebar"/>) have the same groups: same headings and
    /// the same sections with the same titles, in the same order. Every state event brings new lists;
    /// only a different sidebar (another language) is worth rebuilding the items for, the dots are
    /// updated in place.
    /// </summary>
    public static bool Same(IReadOnlyList<SidebarGroupView>? shown, IReadOnlyList<SidebarGroupView>? next) =>
        shown is not null
        && next is not null
        && shown.Count == next.Count
        && shown.Zip(next).All(pair => pair.First.Heading == pair.Second.Heading
            && pair.First.Sections.SequenceEqual(pair.Second.Sections));
}
