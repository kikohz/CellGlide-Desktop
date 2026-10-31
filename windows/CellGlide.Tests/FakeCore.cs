using System.Text.Json;
using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;

namespace CellGlide.Tests;

/// <summary>An ICore that records what is sent and raises whatever a test asks for.</summary>
sealed class FakeCore(ReplyResult? autoReply = null) : ICore
{
    readonly Lock _gate = new();
    ulong _next;

    public List<Command> Sent { get; } = [];

    public List<ulong> SentIds { get; } = [];

    public event Action<CoreEvent>? EventReceived;

    public ulong Send(Command command)
    {
        ulong id;
        lock (_gate)
        {
            id = ++_next;
            Sent.Add(command);
            SentIds.Add(id);
        }
        // Answers inline, before Send returns: the hardest case for the hub.
        if (autoReply is not null)
        {
            Raise(new ReplyEvent(id, autoReply));
        }
        return id;
    }

    public string Tr(string key, IReadOnlyDictionary<string, string>? args = null) => key;

    public void Raise(CoreEvent ev) => EventReceived?.Invoke(ev);

    public void Dispose()
    {
    }
}

/// <summary>Views read from the contract fixtures.</summary>
static class Samples
{
    public static OverviewView Overview() =>
        Assert.IsType<StateEvent>(Event("state_connected.json")).View;

    public static ChromeView Chrome() =>
        Assert.IsType<ServiceGoneEvent>(Event("service_gone_stopped.json")).Chrome;

    public static CoreEvent Event(string name) =>
        JsonSerializer.Deserialize(File.ReadAllText(Fixtures.Path("events", name)), CoreJsonContext.Default.CoreEvent)!;
}

/// <summary>crates/cg-ffi/tests/fixtures, found from the test's own directory.</summary>
static class Fixtures
{
    public static readonly string Root = FindRoot();

    public static string Path(string kind, string name) => System.IO.Path.Combine(Root, kind, name);

    public static IEnumerable<string> Files(string kind) =>
        Directory.EnumerateFiles(System.IO.Path.Combine(Root, kind), "*.json").Order(StringComparer.Ordinal);

    static string FindRoot()
    {
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null; dir = dir.Parent)
        {
            var candidate = System.IO.Path.Combine(dir.FullName, "crates", "cg-ffi", "tests", "fixtures");
            if (Directory.Exists(candidate))
            {
                return candidate;
            }
        }
        throw new DirectoryNotFoundException("crates/cg-ffi/tests/fixtures not found above " + AppContext.BaseDirectory);
    }
}
