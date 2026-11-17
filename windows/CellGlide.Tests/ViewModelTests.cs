using System.Text.Json;
using System.Text.Json.Nodes;
using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;

namespace CellGlide.Tests;

public class ViewModelTests
{
    static (FakeCore Core, EventHub Hub) Fake(ReplyResult? autoReply = null)
    {
        var core = new FakeCore(autoReply);
        return (core, new EventHub(core, new InlineDispatcher()));
    }

    [Fact]
    public void Shell_takes_state_and_service_gone()
    {
        var (core, hub) = Fake();
        var shell = new ShellViewModel(hub);
        core.Raise(new StateEvent(Samples.Overview()));
        Assert.Equal(Samples.Overview().Title, shell.Overview!.Title);
        // ServiceGone carries the chrome (Task 9/11); the shell takes it as it comes.
        core.Raise(new ServiceGoneEvent("gone", false, Samples.Chrome()));
        Assert.Equal("gone", shell.ServiceDown!.Text);
        Assert.Equal(Samples.Chrome().Tray, shell.ServiceDown.Chrome.Tray);
        Assert.Null(shell.Overview);
        core.Raise(new StateEvent(Samples.Overview()));
        Assert.Null(shell.ServiceDown);
    }

    [Fact]
    public void Chrome_follows_state_and_service_gone()
    {
        var (core, hub) = Fake();
        var shell = new ShellViewModel(hub);
        var changed = new List<string?>();
        shell.PropertyChanged += (_, e) => changed.Add(e.PropertyName);
        Assert.Null(shell.Chrome);
        core.Raise(new StateEvent(Samples.Overview()));
        Assert.True(SameJson(Samples.Overview().Chrome, shell.Chrome!));
        Assert.Contains(nameof(ShellViewModel.Chrome), changed);
        changed.Clear();
        core.Raise(new ServiceGoneEvent("gone", false, Samples.Chrome()));
        Assert.True(SameJson(Samples.Chrome(), shell.Chrome!));
        Assert.Contains(nameof(ShellViewModel.Chrome), changed);
        changed.Clear();
        core.Raise(new StateEvent(Samples.Overview()));
        Assert.True(SameJson(Samples.Overview().Chrome, shell.Chrome!));
        Assert.Contains(nameof(ShellViewModel.Chrome), changed);
    }

    [Fact]
    public async Task Now_is_never_null_and_follows_state_service_gone_and_errors()
    {
        // x:Bind skips a function binding whose argument ends in null, so the pages bind to Now.
        var (core, hub) = Fake(autoReply: new ReplyErr("refused"));
        var shell = new ShellViewModel(hub);
        var changed = new List<string?>();
        shell.PropertyChanged += (_, e) => changed.Add(e.PropertyName);
        Assert.Equal(new ShellNow(null, null, null), shell.Now);
        Assert.Null(shell.Now.Chrome);

        var state = new StateEvent(Samples.Overview());
        core.Raise(state);
        Assert.Same(shell.Overview, shell.Now.Overview);
        Assert.Null(shell.Now.ServiceDown);
        Assert.Same(shell.Chrome, shell.Now.Chrome);
        Assert.Contains(nameof(ShellViewModel.Now), changed);

        changed.Clear();
        var gone = new ServiceGoneEvent("gone", false, Samples.Chrome());
        core.Raise(gone);
        Assert.Null(shell.Now.Overview);
        Assert.Same(gone, shell.Now.ServiceDown);
        Assert.Same(gone.Chrome, shell.Now.Chrome);
        Assert.Contains(nameof(ShellViewModel.Now), changed);

        var errors = new List<string?>();
        shell.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName == nameof(ShellViewModel.Now))
            {
                errors.Add(shell.Now.Error);
            }
        };
        await shell.ToggleCommand.ExecuteAsync(null);
        Assert.Equal("refused", shell.Now.Error);
        Assert.Same(gone, shell.Now.ServiceDown);
        // The next command clears it first, and Now says so too.
        await shell.ToggleCommand.ExecuteAsync(null);
        Assert.Equal(["refused", null, "refused"], errors);
    }

    // ChromeView's lists are IReadOnlyList (compared by reference) and each sample read is a new
    // instance, so compare by JSON meaning.
    static bool SameJson(ChromeView a, ChromeView b) => JsonNode.DeepEquals(
        JsonSerializer.SerializeToNode(a, CoreJsonContext.Default.ChromeView),
        JsonSerializer.SerializeToNode(b, CoreJsonContext.Default.ChromeView));

    [Fact]
    public async Task Toggle_sends_the_toggle_command()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        await new ShellViewModel(hub).ToggleCommand.ExecuteAsync(null);
        Assert.IsType<ToggleCommand>(Assert.Single(core.Sent));
    }

    [Fact]
    public async Task Subscribe_sends_subscribe()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        await new ShellViewModel(hub).SubscribeAsync();
        Assert.IsType<SubscribeCommand>(Assert.Single(core.Sent));
    }

    [Fact]
    public async Task Failed_command_sets_error_text()
    {
        var (_, hub) = Fake(autoReply: new ReplyErr("服务已停止"));
        var checks = new ChecksViewModel(hub);
        await checks.RunCommand.ExecuteAsync(null);
        Assert.Equal("服务已停止", checks.Error);
        Assert.Null(checks.Rows);
    }

    [Fact]
    public async Task Checks_run_the_doctor_and_take_its_rows()
    {
        var reply = Assert.IsType<ReplyEvent>(Samples.Event("reply_checks.json"));
        var (core, hub) = Fake(autoReply: reply.Result);
        var checks = new ChecksViewModel(hub);
        await checks.RunCommand.ExecuteAsync(null);
        Assert.IsType<DoctorRequest>(Assert.IsType<RequestCommand>(Assert.Single(core.Sent)).Request);
        Assert.Same(((ChecksValue)((ReplyOk)reply.Result).Value).Rows, checks.Rows);
        Assert.Null(checks.Error);
    }

    [Fact]
    public async Task About_takes_the_reply()
    {
        var reply = Assert.IsType<ReplyEvent>(Samples.Event("reply_about.json"));
        var (core, hub) = Fake(autoReply: reply.Result);
        var about = new AboutViewModel(hub);
        Assert.Null(about.About);
        await about.LoadCommand.ExecuteAsync(null);
        Assert.IsType<GetAboutCommand>(Assert.Single(core.Sent));
        Assert.Equal("https://t.me/cellglide", about.About!.TelegramUrl);
        Assert.Equal("t.me/cellglide", about.About.TelegramLabel);
        Assert.Contains("CommunityToolkit.Mvvm", about.About.Licenses);
        Assert.Null(about.Error);
    }

    [Fact]
    public async Task About_failure_leaves_it_unset_with_the_sentence()
    {
        var (_, hub) = Fake(autoReply: new ReplyErr("服务已停止"));
        var about = new AboutViewModel(hub);
        await about.LoadCommand.ExecuteAsync(null);
        Assert.Null(about.About);
        Assert.Equal("服务已停止", about.Error);
    }

    [Fact]
    public async Task Speed_start_takes_include_wifi_from_the_prefs()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        var settings = new SettingsViewModel(hub);
        var speed = new SpeedViewModel(hub, settings);
        core.Raise(new PrefsEvent(new Prefs(false, false, false)));
        await speed.StartCommand.ExecuteAsync(true);
        Assert.Equal(new SpeedStartCommand(false, true, null), Assert.Single(core.Sent));

        core.Raise(new PrefsEvent(new Prefs(false, true, false)));
        await speed.StartCommand.ExecuteAsync(false);
        Assert.Equal(new SpeedStartCommand(true, false, null), core.Sent[1]);
    }

    [Fact]
    public async Task Speed_start_reads_the_prefs_when_none_came()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new PrefsValue(new Prefs(false, true, false))));
        var speed = new SpeedViewModel(hub, new SettingsViewModel(hub));
        await speed.StartCommand.ExecuteAsync(false);
        Assert.IsType<GetPrefsCommand>(core.Sent[0]);
        Assert.Equal(new SpeedStartCommand(true, false, null), core.Sent[1]);
    }

    [Fact]
    public async Task Speed_cancel_and_clear_send_their_commands()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        var speed = new SpeedViewModel(hub, new SettingsViewModel(hub));
        await speed.CancelCommand.ExecuteAsync(null);
        await speed.ClearHistoryCommand.ExecuteAsync(null);
        Assert.IsType<SpeedCancelCommand>(core.Sent[0]);
        Assert.IsType<SpeedHistoryClearCommand>(core.Sent[1]);
        var view = Assert.IsType<SpeedProgressEvent>(Samples.Event("speed_progress_data_warning.json")).View;
        core.Raise(new SpeedProgressEvent(view));
        Assert.Same(view, speed.View);
    }

    [Fact]
    public void Data_warning_is_requested_once_per_transition()
    {
        var (core, hub) = Fake();
        var speed = new SpeedViewModel(hub, new SettingsViewModel(hub));
        var asked = 0;
        speed.DataWarningRequested += () => asked++;
        var warning = Samples.Event("speed_progress_data_warning.json");
        core.Raise(warning);
        core.Raise(warning);                                          // same dialog, next frame
        Assert.Equal(1, asked);
        core.Raise(Samples.Event("speed_progress_latency.json"));     // dialog gone
        core.Raise(warning);
        Assert.Equal(2, asked);
    }

    [Fact]
    public async Task Continuing_with_dont_ask_again_saves_the_prefs_then_starts_confirmed()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        var speed = new SpeedViewModel(hub, new SettingsViewModel(hub));
        core.Raise(new PrefsEvent(new Prefs(false, true, false)));
        await speed.AnswerDataWarningAsync(DataWarningAnswer.Continue, dontAskAgain: true);
        Assert.Equal(
            [new SetPrefsCommand(new Prefs(true, true, false)), new SpeedStartCommand(true, true, null)],
            core.Sent);
    }

    [Fact]
    public async Task Continuing_without_dont_ask_again_only_starts_confirmed()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        var speed = new SpeedViewModel(hub, new SettingsViewModel(hub));
        core.Raise(new PrefsEvent(new Prefs(false, false, false)));
        await speed.AnswerDataWarningAsync(DataWarningAnswer.Continue, dontAskAgain: false);
        Assert.Equal(new SpeedStartCommand(false, true, null), Assert.Single(core.Sent));
    }

    [Fact]
    public async Task Dismissing_the_data_warning_cancels_and_saves_nothing()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        var speed = new SpeedViewModel(hub, new SettingsViewModel(hub));
        core.Raise(new PrefsEvent(new Prefs(false, true, false)));
        // The box ticked, then Cancel or Esc: not a reason to stop asking.
        await speed.AnswerDataWarningAsync(DataWarningAnswer.Dismiss, dontAskAgain: true);
        Assert.IsType<SpeedCancelCommand>(Assert.Single(core.Sent));
    }

    [Fact]
    public async Task A_warning_the_dll_withdrew_sends_nothing()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        var speed = new SpeedViewModel(hub, new SettingsViewModel(hub));
        core.Raise(new PrefsEvent(new Prefs(false, true, false)));
        await speed.AnswerDataWarningAsync(DataWarningAnswer.Withdrawn, dontAskAgain: true);
        Assert.Empty(core.Sent);
    }

    [Fact]
    public async Task Leak_start_cancel_and_view()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        var leak = new LeakViewModel(hub);
        await leak.StartCommand.ExecuteAsync(null);
        await leak.CancelCommand.ExecuteAsync(null);
        Assert.IsType<LeakStartCommand>(core.Sent[0]);
        Assert.IsType<LeakCancelCommand>(core.Sent[1]);
        var view = Assert.IsType<LeakProgressEvent>(Samples.Event("leak_progress_result.json")).View;
        core.Raise(new LeakProgressEvent(view));
        Assert.Same(view, leak.View);
        Assert.NotNull(leak.Checks);
    }

    [Fact]
    public async Task History_loads_the_day_asked_for()
    {
        var reply = Assert.IsType<ReplyEvent>(Samples.Event("reply_history.json"));
        var (core, hub) = Fake(autoReply: reply.Result);
        var history = new HistoryViewModel(hub);
        await history.LoadCommand.ExecuteAsync("2026-10-27");
        await history.LoadCommand.ExecuteAsync(null);
        Assert.Equal(new HistoryRequest("2026-10-27"), Assert.IsType<RequestCommand>(core.Sent[0]).Request);
        Assert.Equal(new HistoryRequest(null), Assert.IsType<RequestCommand>(core.Sent[1]).Request);
        Assert.Same(((HistoryValue)((ReplyOk)reply.Result).Value).View, history.View);
    }

    [Fact]
    public async Task Diagnostics_prepare_send_and_save()
    {
        var core = new ScriptedCore();
        var hub = new EventHub(core, new InlineDispatcher());
        var diagnostics = new DiagnosticsViewModel(hub);
        Assert.False(diagnostics.SendCommand.CanExecute(null));

        core.Next = new ReplyOk(new DiagnosticsValue("{}", "cellglide.json", "2 B in total"));
        await diagnostics.PrepareCommand.ExecuteAsync(new DiagnosticsInput(DiagRange.Week, "a@b.co", "hi"));
        Assert.Equal(new DiagnosticsPrepareCommand(DiagRange.Week, "a@b.co", "hi"), core.Sent[0]);
        Assert.Equal("{}", diagnostics.Json);
        Assert.Equal("cellglide.json", diagnostics.FileName);
        Assert.True(diagnostics.SendCommand.CanExecute(null));

        core.Next = new ReplyOk(new SavedValue(@"C:\Users\me\Downloads\cellglide.json"));
        await diagnostics.SaveCommand.ExecuteAsync(null);
        Assert.Equal(new DiagnosticsSaveCommand("{}", null, false), core.Sent[1]);
        Assert.Equal(@"C:\Users\me\Downloads\cellglide.json", diagnostics.SavedPath);

        // A file the user chose and confirmed replacing in the save dialog.
        core.Next = new ReplyOk(new SavedValue(@"D:\report.json"));
        await diagnostics.SaveCommand.ExecuteAsync(new DiagnosticsSaveTarget(@"D:\report.json", true));
        Assert.Equal(new DiagnosticsSaveCommand("{}", @"D:\report.json", true), core.Sent[2]);
        Assert.Equal(@"D:\report.json", diagnostics.SavedPath);

        // A failed send keeps the bundle, so the same bytes can be sent again.
        core.Next = new ReplyErr("no network");
        await diagnostics.SendCommand.ExecuteAsync(null);
        Assert.Equal("no network", diagnostics.Error);
        Assert.Equal("{}", diagnostics.Json);

        core.Next = new ReplyOk(new SentValue("R-123"));
        await diagnostics.SendCommand.ExecuteAsync(null);
        Assert.Equal(new DiagnosticsSendCommand("{}"), core.Sent[4]);
        Assert.Equal("R-123", diagnostics.Code);
        // A sent bundle is done with: another report needs a new preview (cg-gui pages/diagnostics.rs).
        Assert.Null(diagnostics.Json);
        Assert.False(diagnostics.SendCommand.CanExecute(null));
    }

    [Fact]
    public async Task Diagnostics_size_comes_with_the_preview()
    {
        var core = new ScriptedCore();
        var hub = new EventHub(core, new InlineDispatcher());
        var diagnostics = new DiagnosticsViewModel(hub);
        core.Next = new ReplyOk(new DiagnosticsValue("{}", "cellglide.json", "2 B in total"));
        await diagnostics.PrepareCommand.ExecuteAsync(new DiagnosticsInput(DiagRange.Day, "", ""));
        Assert.Equal("2 B in total", diagnostics.SizeText);
        // It goes with the bundle.
        diagnostics.Discard();
        Assert.Null(diagnostics.SizeText);
    }

    [Fact]
    public async Task Changing_the_form_discards_the_previewed_bundle()
    {
        var core = new ScriptedCore();
        var hub = new EventHub(core, new InlineDispatcher());
        var diagnostics = new DiagnosticsViewModel(hub);
        core.Next = new ReplyOk(new DiagnosticsValue("{}", "cellglide.json", "2 B in total"));
        await diagnostics.PrepareCommand.ExecuteAsync(new DiagnosticsInput(DiagRange.Day, "", ""));
        core.Next = new ReplyOk(new SavedValue(@"C:\d\cellglide.json"));
        await diagnostics.SaveCommand.ExecuteAsync(null);
        diagnostics.Discard();
        Assert.Null(diagnostics.Json);
        Assert.Null(diagnostics.FileName);
        Assert.False(diagnostics.SendCommand.CanExecute(null));
        Assert.False(diagnostics.SaveCommand.CanExecute(null));
        // What was already saved stays saved, with its folder still one click away.
        Assert.Equal(@"C:\d\cellglide.json", diagnostics.SavedPath);
        Assert.Equal(2, core.Sent.Count);
    }

    [Fact]
    public async Task Refresh_reloads_only_today()
    {
        var reply = Assert.IsType<ReplyEvent>(Samples.Event("reply_history_today.json"));
        var (core, hub) = Fake(autoReply: reply.Result);
        var history = new HistoryViewModel(hub);
        await history.LoadCommand.ExecuteAsync(null);
        await history.RefreshCommand.ExecuteAsync(null);
        Assert.Equal(2, core.Sent.Count);
        await history.LoadCommand.ExecuteAsync("2026-10-29");
        await history.RefreshCommand.ExecuteAsync(null);
        Assert.Equal(3, core.Sent.Count);
        Assert.Equal("2026-10-29", history.RequestedDay);
    }

    [Fact]
    public async Task Landing_on_the_newest_day_follows_today()
    {
        // The sample is 2026-10-30 with nothing after it: the service's today.
        var reply = Assert.IsType<ReplyEvent>(Samples.Event("reply_history_today.json"));
        var (core, hub) = Fake(autoReply: reply.Result);
        var history = new HistoryViewModel(hub);
        await history.LoadCommand.ExecuteAsync("2026-10-30");
        Assert.Null(history.RequestedDay);
        await history.RefreshCommand.ExecuteAsync(null);
        Assert.Equal(new HistoryRequest(null), Assert.IsType<RequestCommand>(core.Sent[1]).Request);
    }

    [Fact]
    public async Task History_refresh_waits_for_a_load_and_late_answers_are_dropped()
    {
        var (core, hub) = Fake();
        var history = new HistoryViewModel(hub);
        static HistoryView Day(string day, string? next) => new(day, day, null, next, [], [], [], []);
        var today = history.LoadCommand.ExecuteAsync(null);
        // A load is out: the refresh asks nothing.
        await history.RefreshCommand.ExecuteAsync(null);
        Assert.Single(core.Sent);
        // The user pages back before today's answer came.
        var older = history.LoadCommand.ExecuteAsync("2026-10-29");
        core.Raise(new ReplyEvent(core.SentIds[1], new ReplyOk(new HistoryValue(Day("2026-10-29", "2026-10-30")))));
        await older;
        core.Raise(new ReplyEvent(core.SentIds[0], new ReplyOk(new HistoryValue(Day("2026-10-30", null)))));
        await today;
        Assert.Equal("2026-10-29", history.View?.Day);
        Assert.Equal("2026-10-29", history.RequestedDay);
    }

    [Fact]
    public async Task A_replaced_history_load_that_fails_says_nothing()
    {
        var (core, hub) = Fake();
        var history = new HistoryViewModel(hub);
        var today = history.LoadCommand.ExecuteAsync(null);
        var older = history.LoadCommand.ExecuteAsync("2026-10-29");
        core.Raise(new ReplyEvent(core.SentIds[1], new ReplyOk(new HistoryValue(new("2026-10-29", "x", null, "2026-10-30", [], [], [], [])))));
        await older;
        // The refresh of today that the page turn replaced fails late: the day on screen is fine.
        core.Raise(new ReplyEvent(core.SentIds[0], new ReplyErr("service is down")));
        await today;
        Assert.Null(history.Error);
        Assert.Equal("2026-10-29", history.View?.Day);
        // The newest load's failure is still said.
        var again = history.LoadCommand.ExecuteAsync("2026-10-28");
        core.Raise(new ReplyEvent(core.SentIds[2], new ReplyErr("service is down")));
        await again;
        Assert.Equal("service is down", history.Error);
    }

    [Fact]
    public async Task Saving_without_a_preview_prepares_first_and_never_allows_a_send()
    {
        var core = new ScriptedCore();
        var hub = new EventHub(core, new InlineDispatcher());
        var diagnostics = new DiagnosticsViewModel(hub);
        var input = new DiagnosticsInput(DiagRange.Day, "a@b.co", "");
        core.Next = new ReplyOk(new DiagnosticsValue("{}", "cellglide.json", "2 B in total"));
        await diagnostics.PrepareToSaveCommand.ExecuteAsync(input);
        Assert.Equal(new DiagnosticsPrepareCommand(DiagRange.Day, "a@b.co", ""), core.Sent[0]);
        Assert.Equal("{}", diagnostics.Json);
        Assert.False(diagnostics.Previewed);
        // A saved bundle was never shown (cg-gui pages/diagnostics.rs): no send.
        Assert.False(diagnostics.SendCommand.CanExecute(null));
        core.Next = new ReplyOk(new SavedValue(@"C:\d\cellglide.json"));
        await diagnostics.SaveCommand.ExecuteAsync(null);
        Assert.Equal(new DiagnosticsSaveCommand("{}", null, false), core.Sent[1]);

        // A bundle on hand is not asked for again.
        await diagnostics.PrepareToSaveCommand.ExecuteAsync(input);
        Assert.Equal(2, core.Sent.Count);
        // Viewing it shows the same bundle without asking the service, and then it may be sent.
        await diagnostics.PrepareCommand.ExecuteAsync(input);
        Assert.Equal(2, core.Sent.Count);
        Assert.True(diagnostics.Previewed);
        Assert.True(diagnostics.SendCommand.CanExecute(null));
    }

    [Fact]
    public async Task A_bundle_built_from_another_form_is_built_again()
    {
        var core = new ScriptedCore();
        var hub = new EventHub(core, new InlineDispatcher());
        var diagnostics = new DiagnosticsViewModel(hub);
        core.Next = new ReplyOk(new DiagnosticsValue("{\"old\":1}", "old.json", "2 B in total"));
        await diagnostics.PrepareCommand.ExecuteAsync(new DiagnosticsInput(DiagRange.Day, "", ""));
        Assert.True(diagnostics.Previewed);

        // The user typed, but the change's event has not come yet (no Discard): View asks for the new form.
        core.Next = new ReplyOk(new DiagnosticsValue("{\"new\":1}", "new.json", "2 B in total"));
        await diagnostics.PrepareCommand.ExecuteAsync(new DiagnosticsInput(DiagRange.Day, "a@b.co", ""));
        Assert.Equal(new DiagnosticsPrepareCommand(DiagRange.Day, "a@b.co", ""), core.Sent[1]);
        Assert.Equal("{\"new\":1}", diagnostics.Json);
        Assert.True(diagnostics.Previewed);

        // The same for Save: another range builds again, and the rebuilt bundle is not previewed.
        core.Next = new ReplyOk(new DiagnosticsValue("{\"week\":1}", "week.json", "2 B in total"));
        await diagnostics.PrepareToSaveCommand.ExecuteAsync(new DiagnosticsInput(DiagRange.Week, "a@b.co", ""));
        Assert.Equal(new DiagnosticsPrepareCommand(DiagRange.Week, "a@b.co", ""), core.Sent[2]);
        Assert.Equal("{\"week\":1}", diagnostics.Json);
        Assert.False(diagnostics.Previewed);
        Assert.False(diagnostics.SendCommand.CanExecute(null));

        // A failed rebuild leaves no bundle of the old form behind.
        core.Next = new ReplyErr("service is down");
        await diagnostics.PrepareToSaveCommand.ExecuteAsync(new DiagnosticsInput(DiagRange.Week, "a@b.co", "note"));
        Assert.Null(diagnostics.Json);
        Assert.False(diagnostics.SaveCommand.CanExecute(null));

        // The same form again uses the bundle on hand.
        core.Next = new ReplyOk(new DiagnosticsValue("{}", "x.json", "2 B in total"));
        await diagnostics.PrepareToSaveCommand.ExecuteAsync(new DiagnosticsInput(DiagRange.Day, "", ""));
        await diagnostics.PrepareCommand.ExecuteAsync(new DiagnosticsInput(DiagRange.Day, "", ""));
        Assert.Equal(5, core.Sent.Count);
        Assert.True(diagnostics.Previewed);
    }

    [Fact]
    public async Task Send_another_and_done_forget_the_last_report()
    {
        var core = new ScriptedCore();
        var hub = new EventHub(core, new InlineDispatcher());
        var diagnostics = new DiagnosticsViewModel(hub);
        core.Next = new ReplyOk(new DiagnosticsValue("{}", "cellglide.json", "2 B in total"));
        await diagnostics.PrepareCommand.ExecuteAsync(new DiagnosticsInput(DiagRange.Day, "", ""));
        core.Next = new ReplyOk(new SavedValue(@"C:\d\cellglide.json"));
        await diagnostics.SaveCommand.ExecuteAsync(null);
        core.Next = new ReplyOk(new SentValue("CG-ABCDE"));
        await diagnostics.SendCommand.ExecuteAsync(null);
        Assert.Equal("CG-ABCDE", diagnostics.Code);
        diagnostics.Reset();
        Assert.Null(diagnostics.Code);
        Assert.Null(diagnostics.SavedPath);
        Assert.Null(diagnostics.Json);
        Assert.False(diagnostics.Previewed);
        Assert.False(diagnostics.SendCommand.CanExecute(null));
    }

    [Fact]
    public async Task Settings_send_one_field_at_a_time()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        var settings = new SettingsViewModel(hub);
        await settings.SetAutoConnectCommand.ExecuteAsync(true);
        await settings.SetPhoneCommand.ExecuteAsync(new SerialPhoneChoice("00008140"));
        await settings.SetStrictDnsCommand.ExecuteAsync(false);
        await settings.SetPrefsCommand.ExecuteAsync(new Prefs(true, false, true));
        Assert.Equal(new SetSettingsRequest(true, null, null), Assert.IsType<RequestCommand>(core.Sent[0]).Request);
        Assert.Equal(new SetSettingsRequest(null, new SerialPhoneChoice("00008140"), null), Assert.IsType<RequestCommand>(core.Sent[1]).Request);
        Assert.Equal(new SetSettingsRequest(null, null, false), Assert.IsType<RequestCommand>(core.Sent[2]).Request);
        Assert.Equal(new SetPrefsCommand(new Prefs(true, false, true)), core.Sent[3]);
        Assert.NotNull(settings.Checks);
    }

    [Fact]
    public async Task Settings_take_prefs_from_the_reply_and_the_event()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new PrefsValue(new Prefs(true, true, true))));
        var settings = new SettingsViewModel(hub);
        Assert.Equal(new Prefs(true, true, true), await settings.LoadAsync());
        core.Raise(new PrefsEvent(new Prefs(false, false, false)));
        Assert.Equal(new Prefs(false, false, false), settings.Prefs);
    }

    [Fact]
    public async Task Speed_start_sends_the_caps_when_set()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        var settings = new SettingsViewModel(hub);
        var speed = new SpeedViewModel(hub, settings);
        Assert.Null(speed.Caps);
        core.Raise(new PrefsEvent(new Prefs(true, true, false)));
        speed.Caps = new SpeedCaps(20_000_000, 10_000_000, 10);
        await speed.StartCommand.ExecuteAsync(true);
        Assert.Equal(new SpeedStartCommand(true, true, new SpeedCaps(20_000_000, 10_000_000, 10)), Assert.Single(core.Sent));
    }

    [Fact]
    public async Task A_command_cancelled_by_dispose_leaves_no_error()
    {
        var (_, hub) = Fake();
        var checks = new ChecksViewModel(hub);
        var run = checks.RunCommand.ExecuteAsync(null);
        hub.Dispose();
        await run.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);
        Assert.Null(checks.Error);
        Assert.Null(checks.Rows);
    }

    [Fact]
    public async Task A_reply_that_cannot_reach_the_ui_is_an_internal_error()
    {
        var core = new FakeCore(new ReplyOk(new DoneValue()));
        var hub = new EventHub(core, new ThrowingDispatcher());
        var shell = new ShellViewModel(hub);
        await shell.ToggleCommand.ExecuteAsync(null);
        // FakeCore's Tr gives back the key.
        Assert.Equal("reason.internal", shell.Error);
    }

    // ---- The wireless connection (Task 10) ----

    static OverviewView StateOf(string fixture) => Assert.IsType<StateEvent>(Samples.Event(fixture)).View;

    static WirelessInfoView InfoReply() =>
        ((WirelessValue)((ReplyOk)Assert.IsType<ReplyEvent>(Samples.Event("reply_wireless.json")).Result).Value).View;

    [Fact]
    public async Task Wireless_commands_are_sent_as_they_are()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        var wireless = new WirelessViewModel(hub);
        await wireless.StartCommand.ExecuteAsync(null);
        await wireless.ConfirmCommand.ExecuteAsync(null);
        await wireless.CancelCommand.ExecuteAsync(null);
        await wireless.ForgetCommand.ExecuteAsync("A1");
        await wireless.RepairCommand.ExecuteAsync("B2");
        await wireless.RegenerateCommand.ExecuteAsync(null);
        await wireless.SetEnabledCommand.ExecuteAsync(true);
        Assert.Equal(
            [
                new WirelessStartCommand(),
                new WirelessConfirmCommand(true),
                new WirelessCancelCommand(),
                new WirelessForgetCommand("A1"),
                new WirelessRepairCommand("B2"),
                new WirelessRegenerateCommand(),
                new RequestCommand(new SetSettingsRequest(null, null, null, true)),
            ],
            core.Sent);
        Assert.Null(wireless.Error);
    }

    [Fact]
    public async Task Start_then_cancel_from_the_steps_card()
    {
        var core = new ScriptedCore();
        var hub = new EventHub(core, new InlineDispatcher());
        var wireless = new WirelessViewModel(hub);
        // Offered: the button only, nothing asked.
        core.Raise(new StateEvent(StateOf("state_wireless_offered.json")));
        Assert.True(wireless.View is { Offered: true, Phase: null, ShowSteps: false });
        Assert.NotNull(wireless.View!.ButtonLabel);
        Assert.Empty(core.Sent);
        await wireless.StartCommand.ExecuteAsync(null);
        Assert.IsType<WirelessStartCommand>(Assert.Single(core.Sent));

        // Waiting: the steps card asks for the network name and password, once.
        core.Next = Assert.IsType<ReplyEvent>(Samples.Event("reply_wireless.json")).Result;
        core.Raise(new StateEvent(StateOf("state_wireless_waiting.json")));
        await wireless.InfoAsked;
        Assert.IsType<WirelessInfoCommand>(core.Sent[1]);
        Assert.Equal(InfoReply().Passphrase, wireless.Info!.Passphrase);
        Assert.Equal(3, wireless.Info.Steps.Count);
        Assert.True(wireless.View!.CanCancel);
        core.Raise(new StateEvent(StateOf("state_wireless_waiting.json")));
        Assert.Equal(2, core.Sent.Count);

        // Cancel; the attempt is gone with the next state, and so is the password.
        core.Next = new ReplyOk(new DoneValue());
        await wireless.CancelCommand.ExecuteAsync(null);
        Assert.IsType<WirelessCancelCommand>(core.Sent[2]);
        core.Raise(new StateEvent(StateOf("state_wireless_offered.json")));
        Assert.Null(wireless.Info);
        Assert.Null(wireless.Code);
    }

    [Fact]
    public async Task A_late_info_reply_is_not_kept_once_the_steps_are_gone()
    {
        var (core, hub) = Fake();
        var wireless = new WirelessViewModel(hub);
        core.Raise(new StateEvent(StateOf("state_wireless_waiting.json")));
        Assert.IsType<WirelessInfoCommand>(Assert.Single(core.Sent));
        // The iPhone came before the reply: pairing, no steps card.
        core.Raise(new StateEvent(StateOf("state_wireless_pairing.json")));
        core.Raise(new ReplyEvent(core.SentIds[0], new ReplyOk(new WirelessValue(InfoReply()))));
        await wireless.InfoAsked;
        Assert.Null(wireless.Info);
    }

    [Fact]
    public void The_code_follows_the_pairing_phase()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        var wireless = new WirelessViewModel(hub);
        core.Raise(new StateEvent(StateOf("state_wireless_pairing.json")));
        var ask = wireless.Code!;
        Assert.Equal("384 271", ask.Code);
        Assert.False(ask.Confirmed);
        Assert.Contains("384 271", ask.Title);
        core.Raise(new StateEvent(StateOf("state_wireless_pairing_confirmed.json")));
        Assert.True(wireless.Code!.Confirmed);
        Assert.Equal("384 271", wireless.Code.Code);
        core.Raise(new StateEvent(StateOf("state_wireless_connected.json")));
        Assert.Null(wireless.Code);
        core.Raise(new StateEvent(StateOf("state_wireless_pairing.json")));
        core.Raise(new ServiceGoneEvent("gone", false, Samples.Chrome()));
        Assert.Null(wireless.Code);
        Assert.Null(wireless.View);
        Assert.Null(wireless.Settings);
    }

    [Fact]
    public async Task Code_answers_confirm_or_cancel_and_the_rest_send_nothing()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        var wireless = new WirelessViewModel(hub);
        await wireless.AnswerCodeAsync(CodeAnswer.Match);
        await wireless.AnswerCodeAsync(CodeAnswer.Dismissed);
        await wireless.AnswerCodeAsync(CodeAnswer.Withdrawn);
        await wireless.AnswerCodeAsync(CodeAnswer.Cancel);
        Assert.Equal([new WirelessConfirmCommand(true), new WirelessCancelCommand()], core.Sent);
    }

    static CodePrompt Prompt(bool confirmed = false, string code = "384 271") =>
        new(code, "Confirm the code " + code + " on the iPhone", "Compare it", confirmed);

    [Fact]
    public void The_code_dialog_opens_on_a_code_and_closes_when_the_phase_moves_on()
    {
        var tracker = new CodeDialogTracker();
        Assert.Equal(CodeDialogStep.None, tracker.Follow(null, canShow: true));
        // The window is hidden: nothing until it is shown.
        Assert.Equal(CodeDialogStep.None, tracker.Follow(Prompt(), canShow: false));
        Assert.Equal(CodeDialogStep.Open, tracker.Follow(Prompt(), canShow: true));
        Assert.Equal(Prompt(), tracker.Shown);
        // Only one: the same prompt again is nothing.
        Assert.Equal(CodeDialogStep.None, tracker.Follow(Prompt(), canShow: true));
        // Confirmed (by the state): the open dialog shows it.
        Assert.Equal(CodeDialogStep.Update, tracker.Follow(Prompt(confirmed: true), canShow: true));
        // The phase left pairing: it closes, and its end sends nothing.
        Assert.Equal(CodeDialogStep.Close, tracker.Follow(null, canShow: true));
        Assert.Null(tracker.Shown);
        // Until the closing dialog has ended, nothing opens.
        Assert.Equal(CodeDialogStep.None, tracker.Follow(Prompt(code: "111 222"), canShow: true));
        Assert.Equal(CodeAnswer.Withdrawn, tracker.Ended(shown: true, cancelled: false));
        Assert.Equal(CodeDialogStep.Open, tracker.Follow(Prompt(code: "111 222"), canShow: true));
    }

    [Fact]
    public void They_match_keeps_the_dialog_open_and_cancel_while_waiting_ends_the_attempt()
    {
        var tracker = new CodeDialogTracker();
        tracker.Follow(Prompt(), canShow: true);
        var shown = tracker.Matched();
        Assert.True(shown!.Confirmed);
        // The state catches up: the same code, confirmed, with the DLL's own sentence: shown as it says.
        var fromDll = Prompt(confirmed: true) with { Detail = "Waiting for the iPhone to confirm too…" };
        Assert.Equal(CodeDialogStep.Update, tracker.Follow(fromDll, canShow: true));
        Assert.Equal(CodeDialogStep.None, tracker.Follow(fromDll, canShow: true));
        // Cancel while waiting for the iPhone: the attempt ends.
        Assert.Equal(CodeAnswer.Cancel, tracker.Ended(shown: true, cancelled: true));
        Assert.Null(tracker.Shown);
    }

    [Fact]
    public void A_confirm_the_service_did_not_take_shows_the_question_again()
    {
        var tracker = new CodeDialogTracker();
        tracker.Follow(Prompt(), canShow: true);
        tracker.Matched();
        // The next state still asks (the confirm failed): the dialog asks again.
        Assert.Equal(CodeDialogStep.Update, tracker.Follow(Prompt(), canShow: true));
        Assert.False(tracker.Shown!.Confirmed);
    }

    [Fact]
    public void Esc_puts_the_code_away_until_asked_for()
    {
        var tracker = new CodeDialogTracker();
        tracker.Follow(Prompt(), canShow: true);
        Assert.Equal(CodeAnswer.Dismissed, tracker.Ended(shown: true, cancelled: false));
        // Not again by itself for the same code (each state would reopen it otherwise).
        Assert.Equal(CodeDialogStep.None, tracker.Follow(Prompt(), canShow: true));
        Assert.Equal(CodeDialogStep.None, tracker.Follow(Prompt(confirmed: true), canShow: true));
        // "Check the Code" brings it back.
        tracker.Ask();
        Assert.Equal(CodeDialogStep.Open, tracker.Follow(Prompt(), canShow: true));
        // Put away again; once the phase moved on, a new code opens by itself.
        tracker.Ended(shown: true, cancelled: false);
        tracker.Follow(null, canShow: true);
        Assert.Equal(CodeDialogStep.Open, tracker.Follow(Prompt(), canShow: true));
    }

    [Fact]
    public void A_code_dialog_that_could_not_be_shown_is_tried_again()
    {
        var tracker = new CodeDialogTracker();
        tracker.Follow(Prompt(), canShow: true);
        // Another dialog was up.
        Assert.Equal(CodeAnswer.Withdrawn, tracker.Ended(shown: false, cancelled: false));
        Assert.Equal(CodeDialogStep.Open, tracker.Follow(Prompt(), canShow: true));
    }

    [Fact]
    public async Task The_switch_is_busy_while_it_is_checked_and_a_refusal_says_why()
    {
        var (core, hub) = Fake();
        var wireless = new WirelessViewModel(hub);
        core.Raise(new StateEvent(StateOf("state_wireless_offered.json")));
        Assert.False(wireless.SwitchBusy);
        var busy = new List<bool>();
        wireless.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName == nameof(WirelessViewModel.SwitchBusy))
            {
                busy.Add(wireless.SwitchBusy);
            }
        };
        // Turning it on: its reply takes a while.
        var on = wireless.SetEnabledCommand.ExecuteAsync(true);
        Assert.True(wireless.SwitchBusy);
        // Meanwhile the service says it is checking.
        core.Raise(new StateEvent(StateOf("state_wireless_failed.json")));
        Assert.True(wireless.Settings!.Checking);
        Assert.True(wireless.SwitchBusy);
        // Refused: the reason; the switch stays busy while the service still says it checks.
        const string refused = "This computer can't open a wireless network for the iPhone: Wi-Fi is turned off";
        core.Raise(new ReplyEvent(core.SentIds[0], new ReplyErr(refused)));
        await on;
        Assert.Equal(refused, wireless.Error);
        Assert.True(wireless.SwitchBusy);
        core.Raise(new StateEvent(StateOf("state_wireless_offered.json")));
        Assert.False(wireless.SwitchBusy);
        Assert.Contains(true, busy);
        Assert.False(busy[^1]);
    }

    [Fact]
    public void Settings_carry_the_paired_iphones_and_whether_an_attempt_runs()
    {
        var (core, hub) = Fake();
        var wireless = new WirelessViewModel(hub);
        core.Raise(new StateEvent(StateOf("state_wireless_waiting.json")));
        var settings = wireless.Settings!;
        Assert.True(settings.Enabled);
        Assert.True(settings.Busy);
        Assert.Equal(2, settings.Paired.Count);
        Assert.Contains(settings.Paired, p => p.Stale);
        // The password is in no state, and never printed.
        Assert.DoesNotContain(InfoReply().Passphrase, File.ReadAllText(Fixtures.Path("events", "state_wireless_waiting.json")));
        Assert.DoesNotContain(InfoReply().Passphrase, InfoReply().ToString());
    }

    sealed class ThrowingDispatcher : IUiDispatcher
    {
        public void Post(Action action) => throw new InvalidOperationException("ui thread gone");
    }

    /// <summary>Answers each command, inline, with whatever <see cref="Next"/> is.</summary>
    sealed class ScriptedCore : ICore
    {
        ulong _next;

        public ReplyResult Next { get; set; } = new ReplyOk(new DoneValue());

        public List<Command> Sent { get; } = [];

        public event Action<CoreEvent>? EventReceived;

        public ulong Send(Command command)
        {
            Sent.Add(command);
            var id = ++_next;
            EventReceived?.Invoke(new ReplyEvent(id, Next));
            return id;
        }

        public void Raise(CoreEvent ev) => EventReceived?.Invoke(ev);

        public string Tr(string key, IReadOnlyDictionary<string, string>? args = null) => key;

        public void Dispose()
        {
        }
    }
}
