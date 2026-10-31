using CellGlide.App.Hosting;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace CellGlide.App.Shell;

/// <summary>
/// A Copy button's short "Copied" moment (cg-gui COPIED_FOR), the page's own feedback state: after a copy that
/// worked (<see cref="Show.Copy"/>), <see cref="Copied"/> holds the copied text for 2 seconds and the button
/// that did it is announced (it carries AutomationProperties.LiveSetting); only the last copy's timer clears
/// it. <paramref name="changed"/> runs on each change, for the owner to relabel with <see cref="SetLabel"/>.
/// </summary>
public sealed class CopyFeedback(Action changed)
{
    /// <summary>How long "Copied" shows before turning back into "Copy".</summary>
    public static readonly TimeSpan For = TimeSpan.FromSeconds(2);

    /// <summary>Numbers the copies: only the latest one's timer puts the label back.</summary>
    int _copies;

    /// <summary>The text just copied, while its button says so.</summary>
    public string? Copied { get; private set; }

    /// <summary>Whether <paramref name="text"/> is the one just copied.</summary>
    public bool IsCopied(string text) => Copied == text;

    /// <summary>Gives <paramref name="button"/> <c>common.copied</c> or <c>common.copy</c>, as its text and its name.</summary>
    public static void SetLabel(Button button, bool copied)
    {
        var label = L.T(copied ? "common.copied" : "common.copy");
        button.Content = label;
        AutomationProperties.SetName(button, label);
    }

    /// <summary>Copies <paramref name="text"/>; when the clipboard took it, says so on <paramref name="announce"/> for 2 seconds.</summary>
    public async void Copy(string text, UIElement announce)
    {
        if (!Show.Copy(text))
        {
            // The clipboard is held by another program right now: nothing copied, nothing claimed.
            return;
        }
        Copied = text;
        var copy = ++_copies;
        changed();
        Show.Announce(announce);
        await Task.Delay(For);
        if (copy == _copies)
        {
            Copied = null;
            changed();
        }
    }

    /// <summary>Forgets a copy at once (its timer then does nothing); the owner relabels.</summary>
    public void Reset()
    {
        Copied = null;
        _copies++;
    }
}
