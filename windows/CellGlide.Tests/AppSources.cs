using System.Text.RegularExpressions;

namespace CellGlide.Tests;

/// <summary>The text keys windows/CellGlide.App names, read from its sources (the tests do not reference the App).</summary>
static partial class AppSources
{
    /// <summary>windows/CellGlide.App, found from the test's own directory.</summary>
    public static readonly string AppDir = FindAppDir();

    /// <summary>The first argument of every L.T('…'), L.T("…") and Tr("…" in the App's *.xaml and *.cs (not bin, obj).</summary>
    public static IReadOnlySet<string> Keys()
    {
        var keys = new HashSet<string>(StringComparer.Ordinal);
        foreach (var file in Files())
        {
            var text = File.ReadAllText(file);
            foreach (var regex in new[] { LT(), Tr() })
            {
                foreach (Match m in regex.Matches(text))
                {
                    keys.Add(m.Groups[1].Value);
                }
            }
        }
        return keys;
    }

    static IEnumerable<string> Files()
    {
        var skip = new[] { "bin", "obj" };
        return Directory.EnumerateFiles(AppDir, "*", SearchOption.AllDirectories)
            .Where(f => f.EndsWith(".xaml", StringComparison.OrdinalIgnoreCase) || f.EndsWith(".cs", StringComparison.OrdinalIgnoreCase))
            .Where(f => !Path.GetRelativePath(AppDir, f).Split(Path.DirectorySeparatorChar).Any(part => skip.Contains(part, StringComparer.OrdinalIgnoreCase)))
            .Order(StringComparer.Ordinal);
    }

    [GeneratedRegex("""L\.T\(\s*['"]([a-z0-9_.]+)['"]""")]
    private static partial Regex LT();

    [GeneratedRegex(@"\bTr\(\s*""([a-z0-9_.]+)""")]
    private static partial Regex Tr();

    static string FindAppDir()
    {
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null; dir = dir.Parent)
        {
            var candidate = Path.Combine(dir.FullName, "windows", "CellGlide.App");
            if (Directory.Exists(candidate))
            {
                return candidate;
            }
        }
        throw new DirectoryNotFoundException("windows/CellGlide.App not found above " + AppContext.BaseDirectory);
    }
}
