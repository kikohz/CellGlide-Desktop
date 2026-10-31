// Mirror of cellglide_native::api::{Event, ReplyResult, ReplyValue} (crates/cg-ffi/src/api.rs).
using System.Text.Json.Serialization;

namespace CellGlide.Core.Models;

/// <summary>What the DLL tells the app (Rust's Event), in order, on the DLL's own thread.</summary>
[JsonPolymorphic(TypeDiscriminatorPropertyName = "type")]
[JsonDerivedType(typeof(ReplyEvent), "reply")]
[JsonDerivedType(typeof(StateEvent), "state")]
[JsonDerivedType(typeof(ServiceGoneEvent), "service_gone")]
[JsonDerivedType(typeof(PhonePluggedEvent), "phone_plugged")]
[JsonDerivedType(typeof(SpeedProgressEvent), "speed_progress")]
[JsonDerivedType(typeof(LeakProgressEvent), "leak_progress")]
[JsonDerivedType(typeof(PrefsEvent), "prefs")]
[JsonDerivedType(typeof(FaultEvent), "fault")]
public abstract record CoreEvent;

/// <summary>The result of command <c>Id</c>.</summary>
public sealed record ReplyEvent(ulong Id, ReplyResult Result) : CoreEvent;

public sealed record StateEvent(OverviewView View) : CoreEvent;

/// <summary>No state for now, and why; the tray, menu, sidebar and dots come with it.</summary>
public sealed record ServiceGoneEvent(string Text, bool NotInGroup, ChromeView Chrome) : CoreEvent;

public sealed record PhonePluggedEvent(string Serial) : CoreEvent;

public sealed record SpeedProgressEvent(SpeedView View) : CoreEvent;

public sealed record LeakProgressEvent(LeakView View) : CoreEvent;

/// <summary>The preferences changed.</summary>
public sealed record PrefsEvent(Prefs Prefs) : CoreEvent;

/// <summary>Something inside the DLL (or reading its event) failed; it carries on.</summary>
public sealed record FaultEvent(string Message) : CoreEvent;

[JsonPolymorphic(TypeDiscriminatorPropertyName = "status")]
[JsonDerivedType(typeof(ReplyOk), "ok")]
[JsonDerivedType(typeof(ReplyErr), "err")]
public abstract record ReplyResult;

public sealed record ReplyOk(ReplyValue Value) : ReplyResult;

/// <summary><c>Message</c> is a sentence for the user, in the current language.</summary>
public sealed record ReplyErr(string Message) : ReplyResult;

[JsonPolymorphic(TypeDiscriminatorPropertyName = "kind")]
[JsonDerivedType(typeof(DoneValue), "done")]
[JsonDerivedType(typeof(ChecksValue), "checks")]
[JsonDerivedType(typeof(HistoryValue), "history")]
[JsonDerivedType(typeof(DiagnosticsValue), "diagnostics")]
[JsonDerivedType(typeof(SentValue), "sent")]
[JsonDerivedType(typeof(SavedValue), "saved")]
[JsonDerivedType(typeof(PrefsValue), "prefs")]
[JsonDerivedType(typeof(AboutValue), "about")]
public abstract record ReplyValue;

public sealed record DoneValue : ReplyValue;

public sealed record ChecksValue(IReadOnlyList<CheckRow> Rows) : ReplyValue;

public sealed record HistoryValue(HistoryView View) : ReplyValue;

public sealed record DiagnosticsValue(string Json, string FileName) : ReplyValue;

/// <summary>The report code the server gave back.</summary>
public sealed record SentValue(string Code) : ReplyValue;

public sealed record SavedValue(string Path) : ReplyValue;

public sealed record PrefsValue(Prefs Prefs) : ReplyValue;

/// <summary>For the About page; <c>Licenses</c> is the Rust list then the NuGet list, each without its generator's header line.</summary>
public sealed record AboutValue(string Version, string TelegramUrl, string Licenses) : ReplyValue;
