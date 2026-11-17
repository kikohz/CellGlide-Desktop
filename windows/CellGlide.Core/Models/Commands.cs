// Mirror of cellglide_native::api::Command (crates/cg-ffi/src/api.rs).
using System.Text.Json.Serialization;

namespace CellGlide.Core.Models;

/// <summary>What the app asks the DLL for; the result comes back as a <see cref="ReplyEvent"/>.</summary>
[JsonPolymorphic(TypeDiscriminatorPropertyName = "type")]
[JsonDerivedType(typeof(SubscribeCommand), "subscribe")]
[JsonDerivedType(typeof(RequestCommand), "request")]
[JsonDerivedType(typeof(ToggleCommand), "toggle")]
[JsonDerivedType(typeof(SpeedStartCommand), "speed_start")]
[JsonDerivedType(typeof(SpeedCancelCommand), "speed_cancel")]
[JsonDerivedType(typeof(SpeedHistoryClearCommand), "speed_history_clear")]
[JsonDerivedType(typeof(LeakStartCommand), "leak_start")]
[JsonDerivedType(typeof(LeakCancelCommand), "leak_cancel")]
[JsonDerivedType(typeof(DiagnosticsPrepareCommand), "diagnostics_prepare")]
[JsonDerivedType(typeof(DiagnosticsSendCommand), "diagnostics_send")]
[JsonDerivedType(typeof(DiagnosticsSaveCommand), "diagnostics_save")]
[JsonDerivedType(typeof(SetLangCommand), "set_lang")]
[JsonDerivedType(typeof(GetPrefsCommand), "get_prefs")]
[JsonDerivedType(typeof(SetPrefsCommand), "set_prefs")]
[JsonDerivedType(typeof(GetAboutCommand), "get_about")]
[JsonDerivedType(typeof(WirelessStartCommand), "wireless_start")]
[JsonDerivedType(typeof(WirelessCancelCommand), "wireless_cancel")]
[JsonDerivedType(typeof(WirelessConfirmCommand), "wireless_confirm")]
[JsonDerivedType(typeof(WirelessForgetCommand), "wireless_forget")]
[JsonDerivedType(typeof(WirelessRepairCommand), "wireless_repair")]
[JsonDerivedType(typeof(WirelessRegenerateCommand), "wireless_regenerate")]
[JsonDerivedType(typeof(WirelessInfoCommand), "wireless_info")]
public abstract record Command;

/// <summary>Keep sending state (and ServiceGone while the service is away).</summary>
public sealed record SubscribeCommand : Command;

/// <summary>One request to the service.</summary>
public sealed record RequestCommand(Request Request) : Command;

/// <summary>Connect or disconnect, whichever the last state calls for.</summary>
public sealed record ToggleCommand : Command;

public sealed record SpeedStartCommand(bool IncludeWifi, bool Confirmed, SpeedCaps? Caps) : Command;

/// <summary>Cancels a running test; also how the data warning is dismissed.</summary>
public sealed record SpeedCancelCommand : Command;

public sealed record SpeedHistoryClearCommand : Command;

public sealed record LeakStartCommand : Command;

public sealed record LeakCancelCommand : Command;

public sealed record DiagnosticsPrepareCommand(DiagRange Range, string Email, string Note) : Command;

public sealed record DiagnosticsSendCommand(string Json) : Command;

/// <summary>Save to <c>Path</c>, or to the Downloads folder when null. An existing file at <c>Path</c> is replaced only
/// with <c>Overwrite</c>, set once the user confirmed replacing it.</summary>
public sealed record DiagnosticsSaveCommand(string Json, string? Path, bool Overwrite) : Command;

/// <summary>zh-CN, en, ...; empty for the system's language.</summary>
public sealed record SetLangCommand(string Lang) : Command;

public sealed record GetPrefsCommand : Command;

public sealed record SetPrefsCommand(Prefs Prefs) : Command;

/// <summary>The version, the Telegram channel and the third-party licenses, for the About page.</summary>
public sealed record GetAboutCommand : Command;

/// <summary>"Connect Wirelessly": opens the access point and waits for the iPhone.</summary>
public sealed record WirelessStartCommand : Command;

/// <summary>Ends the wireless attempt or session and closes the access point.</summary>
public sealed record WirelessCancelCommand : Command;

/// <summary>The user's answer to the code; the app sends only true (a cancel is <see cref="WirelessCancelCommand"/>).</summary>
public sealed record WirelessConfirmCommand(bool Matches) : Command;

/// <summary>Removes a paired iPhone; a wireless session with it ends first.</summary>
public sealed record WirelessForgetCommand(string PeerId) : Command;

/// <summary>Pairs a paired iPhone by code again, replacing its pairing once saved.</summary>
public sealed record WirelessRepairCommand(string PeerId) : Command;

/// <summary>A new network name and password; refused while an attempt runs.</summary>
public sealed record WirelessRegenerateCommand : Command;

/// <summary>The network name, password and steps for the steps card, and the paired iPhones (<see cref="WirelessValue"/>).</summary>
public sealed record WirelessInfoCommand : Command;

/// <summary>Limits for a small speed test.</summary>
public sealed record SpeedCaps(ulong DownloadBytes, ulong UploadBytes, uint Seconds);
