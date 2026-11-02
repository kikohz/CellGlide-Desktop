using CellGlide.Core.Desktop;

namespace CellGlide.Tests;

/// <summary>Starting Apple Devices: the pace (once a minute, three times an episode), the log, and failures kept in.</summary>
public class AppleDevicesTests
{
    /// <summary>Counts the calls; each answers with what <see cref="Next"/> says.</summary>
    sealed class FakeLauncher : IAppleDevicesLauncher
    {
        public int Started { get; private set; }

        public int Opened { get; private set; }

        /// <summary>What the next call does: complete (null), throw at once, or return this task.</summary>
        public Func<Task>? Next { get; set; }

        public Task StartInBackgroundAsync()
        {
            Started++;
            return Next?.Invoke() ?? Task.CompletedTask;
        }

        public Task OpenAsync()
        {
            Opened++;
            return Next?.Invoke() ?? Task.CompletedTask;
        }
    }

    sealed class Clock
    {
        public DateTimeOffset Now { get; set; } = new(2026, 11, 2, 12, 0, 0, TimeSpan.Zero);

        public void Add(int seconds) => Now = Now.AddSeconds(seconds);
    }

    static (AppleDevicesStarter Starter, FakeLauncher Launcher, Clock Clock, List<string> Log) Make()
    {
        var launcher = new FakeLauncher();
        var clock = new Clock();
        var log = new List<string>();
        return (new AppleDevicesStarter(launcher, () => clock.Now, log.Add), launcher, clock, log);
    }

    [Fact]
    public async Task At_most_one_attempt_a_minute_and_three_an_episode()
    {
        var (starter, launcher, clock, log) = Make();
        // Nothing asked: nothing done, nothing logged.
        await starter.Update(false);
        Assert.Equal(0, launcher.Started);
        Assert.Empty(log);

        await starter.Update(true);
        Assert.Equal(1, launcher.Started);
        // Every few seconds the same word: no new attempt within the minute.
        for (var i = 0; i < 5; i++)
        {
            clock.Add(10);
            await starter.Update(true);
        }
        Assert.Equal(1, launcher.Started);
        clock.Add(10);
        await starter.Update(true);
        Assert.Equal(2, launcher.Started);
        clock.Add(60);
        await starter.Update(true);
        Assert.Equal(3, launcher.Started);
        // The third was the last of this episode, however long it lasts.
        for (var i = 0; i < 10; i++)
        {
            clock.Add(60);
            await starter.Update(true);
        }
        Assert.Equal(3, launcher.Started);
        Assert.Equal(3, starter.Attempts);
        Assert.Single(log, l => l.Contains("giving up"));
        Assert.Equal(3, log.Count(l => l.Contains(": done")));
        Assert.All(log, l => Assert.StartsWith("apple-devices: ", l));
        Assert.Contains(log, l => l.Contains("attempt 1/3"));
        Assert.Contains(log, l => l.Contains("attempt 3/3"));
    }

    [Fact]
    public async Task A_new_episode_starts_counting_again()
    {
        var (starter, launcher, clock, log) = Make();
        await starter.Update(true);
        clock.Add(60);
        await starter.Update(true);
        Assert.Equal(2, launcher.Started);
        // Apple's service answers: the episode is over (and said so).
        clock.Add(5);
        await starter.Update(false);
        Assert.Equal(0, starter.Attempts);
        Assert.Contains(log, l => l.Contains("no longer needs starting (after 2 attempt(s))"));
        // It stops again, a little later: a new episode, attempted at once.
        clock.Add(5);
        await starter.Update(true);
        Assert.Equal(3, launcher.Started);
        clock.Add(60);
        await starter.Update(true);
        clock.Add(60);
        await starter.Update(true);
        clock.Add(60);
        await starter.Update(true);
        Assert.Equal(5, launcher.Started);
        // The end of an episode is said once.
        log.Clear();
        await starter.Update(false);
        await starter.Update(false);
        Assert.Single(log);
    }

    [Fact]
    public async Task Failures_are_logged_and_kept_in()
    {
        var (starter, launcher, clock, log) = Make();
        // Thrown at once, or a faulted task: the same.
        launcher.Next = () => throw new InvalidOperationException("class not registered");
        await starter.Update(true);
        Assert.Contains(log, l => l.Contains("attempt 1/3: failed: InvalidOperationException") && l.Contains("class not registered"));
        launcher.Next = () => Task.FromException(new System.Runtime.InteropServices.COMException("denied", unchecked((int)0x80070005)));
        clock.Add(60);
        await starter.Update(true);
        Assert.Contains(log, l => l.Contains("attempt 2/3: failed: COMException 0x80070005"));
        // A failed attempt still counts.
        Assert.Equal(2, starter.Attempts);
        // The button's failure, too.
        await starter.OpenAsync();
        Assert.Equal(1, launcher.Opened);
        Assert.Contains(log, l => l.Contains("opening Apple Devices: failed"));
        // And a log that throws stops nothing.
        var throwing = new AppleDevicesStarter(new FakeLauncher(), () => clock.Now, _ => throw new IOException("disk full"));
        await throwing.Update(true);
        await throwing.OpenAsync();
        Assert.Equal(1, throwing.Attempts);
    }

    [Fact]
    public async Task An_attempt_still_running_is_not_started_again()
    {
        var (starter, launcher, clock, log) = Make();
        var hung = new TaskCompletionSource();
        launcher.Next = () => hung.Task;
        var first = starter.Update(true);
        clock.Add(120);
        await starter.Update(true);
        Assert.Equal(1, launcher.Started);
        hung.SetResult();
        await first;
        Assert.Contains(log, l => l.Contains("attempt 1/3: done"));
        launcher.Next = null;
        await starter.Update(true);
        Assert.Equal(2, launcher.Started);
    }

    [Fact]
    public async Task The_button_opens_apple_devices_whatever_the_pace()
    {
        var (starter, launcher, _, log) = Make();
        await starter.OpenAsync();
        await starter.OpenAsync();
        Assert.Equal(2, launcher.Opened);
        Assert.Equal(0, launcher.Started);
        Assert.Equal(2, log.Count(l => l.Contains("opening Apple Devices: done")));
    }

    [Fact]
    public void The_log_appends_timestamped_lines_and_rolls_over()
    {
        var dir = Directory.CreateTempSubdirectory("cg-log-");
        try
        {
            var path = Path.Combine(dir.FullName, "sub", "app.log");
            var log = new FileLog(path, maxBytes: 200);
            log.Write("first");
            log.Write("second");
            var lines = File.ReadAllLines(path);
            Assert.Equal(2, lines.Length);
            Assert.Matches(@"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}[+-]\d\d:\d\d first$", lines[0]);
            Assert.EndsWith(" second", lines[1]);
            for (var i = 0; i < 10; i++)
            {
                log.Write("line " + i + new string('x', 20));
            }
            Assert.True(File.Exists(path + ".1"));
            Assert.True(new FileInfo(path).Length < 200 + 100);
            Assert.Contains("line 9", File.ReadAllText(path));
            // A path it can't write to is no failure.
            new FileLog(Path.Combine(dir.FullName, "sub", "app.log", "not-a-dir", "x.log")).Write("lost");
        }
        finally
        {
            dir.Delete(recursive: true);
        }
    }
}
