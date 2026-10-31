using System.Reflection;
using CellGlide.Core.ViewModels;

namespace CellGlide.Tests;

/// <summary>
/// x:Bind does not call a function again when the path of one of its arguments runs into null (the
/// generated code checks each step), so the App's pages must pass functions values that are never
/// null: <c>Shell.Now</c>, not <c>Shell.Overview</c>.
/// </summary>
public class XBindTests
{
    [Fact]
    public void No_function_binding_takes_a_path_that_can_be_null()
    {
        var bad = AppSources.BindCalls()
            .SelectMany(call => call.Args
                .Where(CanBeNull)
                .Select(arg => $"{call.File}: {call.Function}({arg})"))
            .ToList();
        Assert.Empty(bad);
    }

    [Fact]
    public void A_nullable_shell_path_is_caught()
    {
        var calls = AppSources.BindCalls(
            """<TextBlock Text="{x:Bind s:Show.Title(Shell.Overview), Mode=OneWay}" Visibility="{x:Bind Show.Visible(Shell.Chrome.Menu)}" Tag="{x:Bind F(Shell.Now, 'x', Shell.ToggleCommand)}" />""")
            .ToList();
        Assert.Equal(3, calls.Count);
        Assert.Equal(["Shell.Overview"], calls[0].Args.Where(CanBeNull));
        Assert.Equal(["Shell.Chrome.Menu"], calls[1].Args.Where(CanBeNull));
        Assert.DoesNotContain(calls[2].Args, CanBeNull);
        // The other pages' view models too: their views come only with the DLL's first event.
        Assert.Equal(["Speed.View", "Leak.View.Result"], AppSources.BindCalls("""<A B="{x:Bind F(Speed.View, Leak.View.Result, Speed.StartCommand)}" />""").Single().Args.Where(CanBeNull));
        Assert.Contains(AppSources.BindCalls(), call => call.Args.Contains("Shell.Now"));
    }

    static readonly NullabilityInfoContext Nullability = new();

    /// <summary>The view models the pages name in their bindings, by the name they use.</summary>
    static readonly Dictionary<string, Type> Roots = new()
    {
        ["Shell"] = typeof(ShellViewModel),
        ["Speed"] = typeof(SpeedViewModel),
        ["Leak"] = typeof(LeakViewModel),
        ["Settings"] = typeof(SettingsViewModel),
        ["Checks"] = typeof(ChecksViewModel),
    };

    /// <summary>
    /// Whether an argument path through one of the view models meets a property declared nullable
    /// (Overview, ServiceDown, Chrome, Error, View...), or an unknown member. Other roots are not judged.
    /// </summary>
    static bool CanBeNull(string arg)
    {
        var parts = arg.Split('.');
        if (!Roots.TryGetValue(parts[0], out var type) || parts.Length < 2)
        {
            return false;
        }
        foreach (var name in parts.Skip(1))
        {
            if (type.GetProperty(name, BindingFlags.Public | BindingFlags.Instance) is not { } property)
            {
                return true;
            }
            if (!property.PropertyType.IsValueType && Nullability.Create(property).ReadState != NullabilityState.NotNull)
            {
                return true;
            }
            type = property.PropertyType;
        }
        return false;
    }
}
