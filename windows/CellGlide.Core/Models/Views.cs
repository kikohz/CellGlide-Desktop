// Mirrors of the views (crates/cg-ffi/src/views/*.rs) and the cg_ipc::describe::ui types they use
// (crates/cg-ipc/src/describe/ui.rs). Every text is already translated; nothing here is derived.
using System.Text.Json.Serialization;

namespace CellGlide.Core.Models;

// ---- cg_ipc::describe::ui ----

[JsonConverter(typeof(SnakeCaseEnumConverter<Tone>))]
public enum Tone
{
    Green,
    Orange,
    Red,
    Blue,
    Purple,
    Gray,
}

[JsonConverter(typeof(SnakeCaseEnumConverter<Section>))]
public enum Section
{
    Overview,
    SpeedTest,
    DnsLeak,
    History,
    Diagnostics,
    Settings,
    About,
}

[JsonConverter(typeof(SnakeCaseEnumConverter<Badge>))]
public enum Badge
{
    None,
    Warning,
    Critical,
}

[JsonConverter(typeof(SnakeCaseEnumConverter<CheckState>))]
public enum CheckState
{
    Pass,
    Fail,
    Unknown,
}

[JsonConverter(typeof(SnakeCaseEnumConverter<Speeds>))]
public enum Speeds
{
    Hidden,
    NotShared,
    Shared,
}

public sealed record TrayLook(bool Active, bool Alert, bool Wants, string Title);

public sealed record PhoneOption(PhoneChoice Choice, string Label);

public sealed record DnsLine(string Text, Tone Tone);

public sealed record LegendItem(string Label, Tone Tone);

public sealed record StripCell(float X, float Width, Tone Tone, float Alpha);

/// <summary>views/history.rs <c>CheckRowView</c>: <c>ui::CheckRow</c>'s fields and Apple's button.</summary>
public sealed record CheckRow(CheckState State, string Line, string? Fix, bool Copied, AppleDevicesView? AppleDevices);

/// <summary>views/apple.rs: the "Open Apple Devices" button and the sentence beside it.</summary>
public sealed record AppleDevicesView(string ButtonLabel, string Hint);

public sealed record TrailPoint(float X, float Down, float Up);

public sealed record CurvePoint(float X, float Y);

// ---- views/chrome.rs ----

/// <summary>The main window's title, the tray, its menu, the sidebar and its dots.</summary>
public sealed record ChromeView(
    string WindowTitle,
    TrayLook Tray,
    IReadOnlyList<MenuItemView> Menu,
    BadgesView Badges,
    IReadOnlyList<SidebarGroupView> Sidebar);

public sealed record BadgesView(Badge Overview, Badge History, Badge Settings);

public sealed record MenuItemView(MenuKind Kind, string Label, bool Enabled);

[JsonConverter(typeof(SnakeCaseEnumConverter<MenuKind>))]
public enum MenuKind
{
    Version,
    Separator,
    Toggle,
    Open,
    Quit,
}

public sealed record SidebarGroupView(string? Heading, IReadOnlyList<SidebarItemView> Sections);

public sealed record SidebarItemView(Section Section, string Title);

// ---- views/overview.rs ----

public sealed record OverviewView(
    bool Connected,
    bool WantsConnection,
    string Title,
    string Detail,
    Tone StatusTone,
    string ButtonLabel,
    string DeviceSubtitle,
    HealthRowView? Health,
    string? StrictDns,
    PhoneCardView? Phone,
    ChromeView Chrome,
    SettingsView Settings,
    AppleDevicesView? AppleDevices,
    bool StartAppleDevices,
    WirelessView? Wireless);

public sealed record HealthRowView(Tone Tone, string Line);

/// <summary><c>SelectedPhone</c> is an index into <c>Phones</c> (Rust usize).</summary>
public sealed record SettingsView(
    bool AutoConnect,
    bool StrictDns,
    IReadOnlyList<PhoneOption> Phones,
    int SelectedPhone,
    DnsLine Dns,
    string? Proxy,
    WirelessSettingsView Wireless);

public sealed record PhoneCardView(
    string Title,
    string? Carrier,
    string? Radio,
    string? Model,
    BatteryView? Battery,
    Speeds Speeds,
    string? Download,
    string? Upload,
    string? Quality,
    string? BatteryLine,
    HeatView? Heat,
    bool Dim,
    IReadOnlyList<IReadOnlyList<TrailPoint>> Trail);

public sealed record BatteryView(uint Percent, bool Charging, bool LowPower, string Label);

public sealed record HeatView(string Text, bool Severe);

// ---- views/wireless.rs ----

[JsonConverter(typeof(SnakeCaseEnumConverter<WirelessPhase>))]
public enum WirelessPhase
{
    Starting,
    Waiting,
    Pairing,
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

/// <summary>
/// "Connect Wirelessly" (<c>ButtonLabel</c> while <c>Offered</c>) and the attempt: <c>Phase</c> is null without one. <c>Title</c> and
/// <c>Detail</c> are the phase's words (the code dialog's while pairing, the failure's after one). <c>ShowSteps</c>: the steps
/// card, filled from <see cref="WirelessInfoView"/>. <c>Code</c> ("384 271") while pairing; <c>Confirmed</c>: confirmed here,
/// waiting for the iPhone. <c>CanCancel</c>: an attempt runs.
/// </summary>
public sealed record WirelessView(
    bool Offered,
    string? ButtonLabel,
    WirelessPhase? Phase,
    string Title,
    string Detail,
    bool ShowSteps,
    string? Ssid,
    string? Code,
    bool Confirmed,
    string? Hint,
    bool CanCancel);

/// <summary>The settings page's switch (<c>Checking</c>: turning it on is being checked), whether an attempt runs
/// (<c>Busy</c>: Pair Again and New Network Name are off), why it can't work here, and the paired iPhones.</summary>
public sealed record WirelessSettingsView(
    bool Enabled,
    bool Checking,
    bool Busy,
    string? AvailableText,
    IReadOnlyList<PairedPhoneView> Paired);

/// <summary>One paired iPhone: <c>Method</c> is the words for how it was paired; <c>Stale</c>: it must be paired again.</summary>
public sealed record PairedPhoneView(string PeerId, string Name, string Method, bool Stale);

/// <summary>The reply to the wireless info command: the network, its password, the three steps, the note and the paired iPhones.</summary>
public sealed record WirelessInfoView(
    string? Unavailable,
    string Ssid,
    string Passphrase,
    IReadOnlyList<string> Steps,
    string Note,
    IReadOnlyList<PairedPhoneView> Paired)
{
    /// <summary>Never prints the password.</summary>
    public override string ToString() => $"WirelessInfoView {{ Ssid = {Ssid}, Passphrase = <hidden> }}";
}

// ---- views/history.rs ----

public sealed record HistoryView(
    string Day,
    string Label,
    string? PrevDay,
    string? NextDay,
    IReadOnlyList<StripCell> Strip,
    IReadOnlyList<TickView> Ticks,
    IReadOnlyList<HistoryRowView> Rows,
    IReadOnlyList<LegendItem> Legend);

public sealed record TickView(string Label, double At);

public sealed record HistoryRowView(string Time, string Title, Tone Tone, IReadOnlyList<DetailItem> Details);

public sealed record DetailItem(string Label, string Value);

// ---- views/speed.rs ----

[JsonConverter(typeof(SnakeCaseEnumConverter<SpeedDialog>))]
public enum SpeedDialog
{
    DataWarning,
}

public sealed record SpeedView(
    bool Running,
    SpeedDialog? Dialog,
    bool StartEnabled,
    string? StartNote,
    string? Uplink,
    bool OffersWifi,
    IReadOnlyList<SpeedLegView> Legs,
    string? Verdict,
    IReadOnlyList<SpeedRecordView> History,
    bool HistoryFailed);

[JsonConverter(typeof(SnakeCaseEnumConverter<StageView>))]
public enum StageView
{
    Idle,
    Latency,
    Download,
    Upload,
    Done,
    Failed,
    Cancelled,
}

/// <summary><c>Waiting</c>: draw the bar as busy rather than full.</summary>
public sealed record SpeedLegView(
    string Label,
    bool Phone,
    StageView Stage,
    string? StageText,
    string? Error,
    string Numbers,
    double? Latency,
    double? Download,
    double? Upload,
    double Progress,
    bool Waiting,
    IReadOnlyList<CurvePoint> Curve,
    IReadOnlyList<string> Notes);

public sealed record SpeedRecordView(string Date, string? Phone, string? Wifi, string? Carrier);

// ---- views/leak.rs ----

public sealed record LeakView(
    bool Running,
    string StartLabel,
    bool OffersChecks,
    LeakResultView? Result,
    string? Error);

public sealed record LeakResultView(
    Tone Tone,
    string Title,
    string Detail,
    string You,
    IReadOnlyList<ResolverGroupView> Groups,
    string? Subnet);

public sealed record ResolverGroupView(
    string Name,
    string? Location,
    IReadOnlyList<string> Addresses,
    uint? Queries,
    bool Ecs);
