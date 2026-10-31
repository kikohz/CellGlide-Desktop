using CellGlide.Core.Native;

namespace CellGlide.Tests;

/// <summary>Every text the app asks the DLL for exists, in both languages (the real DLL).</summary>
[Collection("native")]
public class AppKeysTests
{
    [Fact]
    public void Every_key_the_app_names_exists_in_both_languages()
    {
        var keys = AppSources.Keys();   // L.T('…') / L.T("…") / Tr("…" in windows/CellGlide.App's *.xaml and *.cs
        Assert.NotEmpty(keys);
        Assert.Contains("gui.title", keys);
        foreach (var lang in new[] { "en", "zh-CN" })
        {
            using var core = NativeCore.Start(lang);
            Assert.DoesNotContain(keys, k => core.Tr(k) == k || core.Tr(k).Length == 0);
        }
    }

    [Fact]
    public void A_missing_key_is_caught()
    {
        // The check above relies on the DLL answering an unknown key with the key itself.
        using var core = NativeCore.Start("en");
        Assert.Equal("gui.no_such_key", core.Tr("gui.no_such_key"));
    }
}
