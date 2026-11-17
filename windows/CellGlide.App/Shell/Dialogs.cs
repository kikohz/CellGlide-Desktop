using Microsoft.UI.Xaml.Controls;

namespace CellGlide.App.Shell;

/// <summary>
/// Every ContentDialog the app shows goes through here. WinUI shows one at a time: a dialog asked for while
/// another is up cannot be shown. <see cref="Closed"/> says when one that was shown has closed, so a dialog that
/// could not be shown (the code dialog) can try again.
/// </summary>
public static class Dialogs
{
    /// <summary>Raised on the UI thread after a dialog that was shown has closed.</summary>
    public static event Action? Closed;

    /// <summary>Shows <paramref name="dialog"/>: how it was closed, or null when it could not be shown (another is up).</summary>
    public static async Task<ContentDialogResult?> ShowAsync(ContentDialog dialog)
    {
        ContentDialogResult result;
        try
        {
            result = await dialog.ShowAsync();
        }
        catch (Exception)
        {
            // Not shown, so nothing closed: raising Closed here could only ask the same failing dialog again.
            return null;
        }
        Closed?.Invoke();
        return result;
    }
}
