using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CellGlide.Core.Native;

namespace CellGlide.Tests;

/// <summary>The DLL holds one runtime, so these run one at a time.</summary>
[CollectionDefinition("native", DisableParallelization = true)]
public class NativeCollection;

/// <summary>The real cellglide_native.dll. Read-only: nothing here writes preferences or touches the network.</summary>
[Collection("native")]
public class NativeTests
{
    [Fact]
    public void Native_init_tr_shutdown()
    {
        using var core = NativeCore.Start("en");
        Assert.Equal("Overview", core.Tr("nav.overview"));
    }

    [Fact]
    public async Task Native_get_prefs_replies()
    {
        using var core = NativeCore.Start("en");
        var hub = new EventHub(core, new InlineDispatcher());
        var ok = Assert.IsType<ReplyOk>(await hub.SendAsync(new GetPrefsCommand()).WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken));
        Assert.IsType<PrefsValue>(ok.Value);
    }

    [Fact]
    public void Native_restart_after_dispose()
    {
        NativeCore.Start("en").Dispose();
        using var again = NativeCore.Start("zh-CN");
        Assert.Equal("概览", again.Tr("nav.overview"));
    }

    [Fact]
    public void Native_second_start_is_refused_with_its_code()
    {
        using var core = NativeCore.Start("en");
        var refused = Assert.Throws<InvalidOperationException>(() => NativeCore.Start("en"));
        Assert.Contains("1", refused.Message);
    }

    [Fact]
    public void Native_send_after_dispose_is_zero()
    {
        var core = NativeCore.Start("en");
        core.Dispose();
        core.Dispose();
        Assert.Equal(0UL, core.Send(new GetPrefsCommand()));
    }

    [Fact]
    public void Native_tr_takes_arguments()
    {
        using var core = NativeCore.Start("en");
        var text = core.Tr("reason.internal", new Dictionary<string, string> { ["detail"] = "xyz" });
        Assert.Contains("xyz", text);
    }

    [Fact]
    public async Task Native_system_language_starts_and_events_stop_after_dispose()
    {
        var events = 0;
        var core = NativeCore.Start("");
        core.EventReceived += _ => Interlocked.Increment(ref events);
        var hub = new EventHub(core, new InlineDispatcher());
        Assert.IsType<ReplyOk>(await hub.SendAsync(new SetLangCommand("en")).WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken));
        core.Dispose();
        var after = Volatile.Read(ref events);
        await Task.Delay(200, TestContext.Current.CancellationToken);
        Assert.Equal(after, Volatile.Read(ref events));
    }
}
