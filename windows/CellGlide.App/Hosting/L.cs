using CellGlide.Core.Dispatch;

namespace CellGlide.App.Hosting;

/// <summary>
/// The DLL's fixed texts for code and XAML (cg-i18n is their only source). Every key named with
/// <c>L.T</c> in this project is checked to exist in both languages (AppKeysTests).
/// </summary>
public static class L
{
    /// <summary>Set once the DLL is running; until then (and after quitting) a text is its key.</summary>
    internal static EventHub? Hub;

    public static string T(string key) => Hub?.Tr(key) ?? key;

    /// <summary>A text with one argument, <paramref name="name"/> = <paramref name="value"/>.</summary>
    public static string T(string key, string name, string value) =>
        Hub?.Tr(key, new Dictionary<string, string> { [name] = value }) ?? key;
}
