using System.Reflection;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.Json.Serialization;
using CellGlide.Core.Models;

namespace CellGlide.Tests;

/// <summary>The C# models read and write every fixture the Rust side wrote, to the same JSON.</summary>
public class ContractTests
{
    public static TheoryData<string> CommandFiles => Files("commands");

    public static TheoryData<string> EventFiles => Files("events");

    static TheoryData<string> Files(string kind) => new(Fixtures.Files(kind));

    static Command ReadCommand(string path) =>
        JsonSerializer.Deserialize(File.ReadAllText(path), CoreJsonContext.Default.Command)!;

    static CoreEvent ReadEvent(string path) =>
        JsonSerializer.Deserialize(File.ReadAllText(path), CoreJsonContext.Default.CoreEvent)!;

    [Theory, MemberData(nameof(CommandFiles))]
    public void Commands_round_trip(string path)
    {
        var text = File.ReadAllText(path);
        var command = JsonSerializer.Deserialize(text, CoreJsonContext.Default.Command)!;
        var again = JsonSerializer.Serialize(command, CoreJsonContext.Default.Command);
        Assert.True(JsonNode.DeepEquals(JsonNode.Parse(text), JsonNode.Parse(again)), $"{path}\n{again}");
    }

    [Theory, MemberData(nameof(EventFiles))]
    public void Events_round_trip(string path)
    {
        var text = File.ReadAllText(path);
        var ev = JsonSerializer.Deserialize(text, CoreJsonContext.Default.CoreEvent)!;
        var again = JsonSerializer.Serialize(ev, CoreJsonContext.Default.CoreEvent);
        Assert.True(JsonNode.DeepEquals(JsonNode.Parse(text), JsonNode.Parse(again)), $"{path}\n{again}");
    }

    [Fact]
    public void Every_derived_type_has_a_fixture()
    {
        var seen = Fixtures.Files("commands").Select(p => ReadCommand(p).GetType())
            .Concat(Fixtures.Files("events").Select(p => ReadEvent(p).GetType()))
            .ToHashSet();
        var declared = typeof(Command).GetCustomAttributes<JsonDerivedTypeAttribute>().Select(a => a.DerivedType)
            .Concat(typeof(CoreEvent).GetCustomAttributes<JsonDerivedTypeAttribute>().Select(a => a.DerivedType));
        Assert.All(declared, t => Assert.Contains(t, seen));
    }

    [Fact]
    public void Every_nested_derived_type_has_a_fixture()
    {
        var seen = new HashSet<Type>();
        foreach (var command in Fixtures.Files("commands").Select(ReadCommand))
        {
            if (command is RequestCommand r)
            {
                seen.Add(r.Request.GetType());
                if (r.Request is SetSettingsRequest { Phone: { } phone })
                {
                    seen.Add(phone.GetType());
                }
            }
        }
        foreach (var ev in Fixtures.Files("events").Select(ReadEvent))
        {
            if (ev is ReplyEvent reply)
            {
                seen.Add(reply.Result.GetType());
                if (reply.Result is ReplyOk ok)
                {
                    seen.Add(ok.Value.GetType());
                }
            }
            if (ev is StateEvent state)
            {
                seen.UnionWith(state.View.Settings.Phones.Select(p => p.Choice.GetType()));
            }
        }
        var declared = new[] { typeof(Request), typeof(ReplyResult), typeof(ReplyValue), typeof(PhoneChoice) }
            .SelectMany(t => t.GetCustomAttributes<JsonDerivedTypeAttribute>().Select(a => a.DerivedType));
        Assert.All(declared, t => Assert.Contains(t, seen));
    }

    [Fact]
    public void Floats_are_written_in_their_shortest_form()
    {
        var text = JsonSerializer.Serialize(Samples.Event("state_connected.json"), CoreJsonContext.Default.CoreEvent);
        Assert.Contains("\"down\":0.8400002", text);
        Assert.Contains("\"up\":0.0480002", text);
    }

    [Fact]
    public void A_missing_note_is_written_as_null()
    {
        var command = new RequestCommand(new DiagnosticsRequest(DiagRange.Day, new Contact("a@b.co", null)));
        Assert.Equal(
            """{"type":"request","request":{"cmd":"diagnostics","range":"day","contact":{"email":"a@b.co","note":null}}}""",
            JsonSerializer.Serialize<Command>(command, CoreJsonContext.Default.Command));
    }

    [Fact]
    public void The_tag_may_come_after_the_fields()
    {
        var ev = JsonSerializer.Deserialize("""{"message":"boom","type":"fault"}""", CoreJsonContext.Default.CoreEvent);
        Assert.Equal("boom", Assert.IsType<FaultEvent>(ev).Message);
    }

    [Fact]
    public void Both_fixture_directories_are_there()
    {
        Assert.NotEmpty(Fixtures.Files("commands"));
        Assert.NotEmpty(Fixtures.Files("events"));
    }
}
