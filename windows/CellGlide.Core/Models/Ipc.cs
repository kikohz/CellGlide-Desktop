// Mirrors of the cg_ipc types the envelope carries (crates/cg-ipc/src/lib.rs, crates/cg-tools/src/prefs.rs).
using System.Text.Json.Serialization;

namespace CellGlide.Core.Models;

/// <summary>cg_ipc::Request: one request to the service, tagged by "cmd".</summary>
[JsonPolymorphic(TypeDiscriminatorPropertyName = "cmd")]
[JsonDerivedType(typeof(HelloRequest), "hello")]
[JsonDerivedType(typeof(StatusRequest), "status")]
[JsonDerivedType(typeof(ConnectRequest), "connect")]
[JsonDerivedType(typeof(DisconnectRequest), "disconnect")]
[JsonDerivedType(typeof(SetSettingsRequest), "set_settings")]
[JsonDerivedType(typeof(DoctorRequest), "doctor")]
[JsonDerivedType(typeof(SubscribeRequest), "subscribe")]
[JsonDerivedType(typeof(HistoryRequest), "history")]
[JsonDerivedType(typeof(DiagnosticsRequest), "diagnostics")]
[JsonDerivedType(typeof(ConnectWirelessRequest), "connect_wireless")]
[JsonDerivedType(typeof(CancelWirelessRequest), "cancel_wireless")]
[JsonDerivedType(typeof(ConfirmCodeRequest), "confirm_code")]
[JsonDerivedType(typeof(WirelessInfoRequest), "wireless_info")]
[JsonDerivedType(typeof(ForgetPhoneRequest), "forget_phone")]
[JsonDerivedType(typeof(RegenerateAccessPointRequest), "regenerate_access_point")]
[JsonDerivedType(typeof(RepairPhoneRequest), "repair_phone")]
public abstract record Request;

public sealed record HelloRequest : Request;

public sealed record StatusRequest : Request;

/// <summary>Connect to <c>Serial</c>, or to any phone when null.</summary>
public sealed record ConnectRequest(string? Serial) : Request;

public sealed record DisconnectRequest : Request;

/// <summary>
/// Only the fields that are not null change. Turning <c>Wireless</c> on first checks that the
/// computer can open the access point; if not, the switch stays off and the reply is an error.
/// </summary>
public sealed record SetSettingsRequest(bool? AutoConnect, PhoneChoice? Phone, bool? StrictDns, bool? Wireless = null) : Request;

public sealed record DoctorRequest : Request;

public sealed record SubscribeRequest : Request;

/// <summary>One day of the timeline, YYYY-MM-DD; null for today.</summary>
public sealed record HistoryRequest(string? Day) : Request;

public sealed record DiagnosticsRequest(DiagRange Range, Contact? Contact) : Request;

/// <summary>Opens the access point and connects to the iPhone that joins it.</summary>
public sealed record ConnectWirelessRequest : Request;

/// <summary>Ends the wireless attempt or session and closes the access point.</summary>
public sealed record CancelWirelessRequest : Request;

/// <summary>Whether the iPhone shows the same code as the one in the pairing phase.</summary>
public sealed record ConfirmCodeRequest(bool Matches) : Request;

/// <summary>The access point's name and passphrase and the paired iPhones.</summary>
public sealed record WirelessInfoRequest : Request;

/// <summary>Removes a pairing; a wireless session with that iPhone ends first.</summary>
public sealed record ForgetPhoneRequest(string PeerId) : Request;

/// <summary>A new network name and passphrase for the access point.</summary>
public sealed record RegenerateAccessPointRequest : Request;

/// <summary>Pairs this paired iPhone by code again, replacing its pairing once saved.</summary>
public sealed record RepairPhoneRequest(string PeerId) : Request;

/// <summary>cg_ipc::PhoneChoice, tagged by "mode".</summary>
[JsonPolymorphic(TypeDiscriminatorPropertyName = "mode")]
[JsonDerivedType(typeof(AutoPhoneChoice), "auto")]
[JsonDerivedType(typeof(SerialPhoneChoice), "serial")]
public abstract record PhoneChoice;

public sealed record AutoPhoneChoice : PhoneChoice;

public sealed record SerialPhoneChoice(string Serial) : PhoneChoice;

[JsonConverter(typeof(SnakeCaseEnumConverter<DiagRange>))]
public enum DiagRange
{
    Day,
    Week,
}

/// <summary>
/// cg_ipc::Contact. Inside the envelope both fields are always written, null when missing
/// (crates/cg-ffi/src/api.rs, request_json), so they follow the context's Never default.
/// </summary>
public sealed record Contact(string? Email, string? Note);

/// <summary>cg_tools::prefs::Prefs: the app's own preferences.</summary>
public sealed record Prefs(bool SpeedConfirmed, bool IncludeWifi, bool OpenOnPlug);
