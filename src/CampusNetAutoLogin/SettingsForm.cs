using System.Drawing;

namespace CampusNetAutoLogin;

public sealed class SettingsForm : Form
{
    private readonly SettingsManager _settingsManager;
    private readonly ICredentialProtector _credentialProtector;
    private readonly StartupManager _startupManager;
    private readonly EventLogger _logger;
    private readonly Func<Task<CleanupResult>> _cleanup;
    private readonly Action _settingsSaved;
    private readonly Action _exitAfterCleanup;

    private readonly TextBox _accountTextBox = new();
    private readonly TextBox _passwordTextBox = new();
    private readonly ComboBox _providerComboBox = new();
    private readonly NumericUpDown _intervalInput = new();
    private readonly NumericUpDown _cooldownInput = new();
    private readonly TextBox _fallbackUrlTextBox = new();
    private readonly CheckBox _autoStartCheckBox = new();
    private readonly CheckBox _successNotificationCheckBox = new();
    private readonly Label _submittedAccountLabel = new();
    private readonly Label _lastLoginLabel = new();
    private readonly Label _networkStatusLabel = new();
    private readonly Button _saveButton = new();
    private bool _allowClose;
    private bool _loading;

    public SettingsForm(
        SettingsManager settingsManager,
        ICredentialProtector credentialProtector,
        StartupManager startupManager,
        EventLogger logger,
        Func<Task<CleanupResult>> cleanup,
        Action settingsSaved,
        Action exitAfterCleanup,
        Icon appIcon)
    {
        _settingsManager = settingsManager;
        _credentialProtector = credentialProtector;
        _startupManager = startupManager;
        _logger = logger;
        _cleanup = cleanup;
        _settingsSaved = settingsSaved;
        _exitAfterCleanup = exitAfterCleanup;

        Text = "校园网自动登录设置";
        Icon = appIcon;
        StartPosition = FormStartPosition.CenterScreen;
        MinimumSize = new Size(620, 560);
        ClientSize = new Size(680, 600);
        ShowInTaskbar = true;
        MaximizeBox = false;
        AutoScaleMode = AutoScaleMode.Dpi;
        Font = new Font("Microsoft YaHei UI", 9F);

        BuildInterface();
        FormClosing += HandleFormClosing;
        Shown += async (_, _) => await ReloadAsync();
    }

    public void ApplyStatus(NetworkStatusSnapshot snapshot)
    {
        _networkStatusLabel.Text = $"{snapshot.DisplayName} — {snapshot.Message}";
        _networkStatusLabel.ForeColor = snapshot.State switch
        {
            NetworkState.Online => Color.ForestGreen,
            NetworkState.LoginFailed or
            NetworkState.LogoutFailed or
            NetworkState.AuthServerUnavailable => Color.Firebrick,
            NetworkState.Paused => Color.DarkOrange,
            _ => SystemColors.ControlText
        };
    }

    public void RefreshLastLogin()
    {
        DateTimeOffset? lastLogin = _settingsManager.Current.LastSuccessfulLoginUtc;
        _lastLoginLabel.Text = lastLogin.HasValue
            ? lastLogin.Value.ToLocalTime().ToString("yyyy-MM-dd HH:mm:ss")
            : "暂无";
    }

    public async Task ReloadAsync()
    {
        if (_loading)
        {
            return;
        }

        _loading = true;
        try
        {
            AppSettings settings = _settingsManager.Current;
            _accountTextBox.Text = settings.Account;
            _passwordTextBox.Clear();
            _passwordTextBox.PlaceholderText = settings.EncryptedPassword.Length > 0
                ? "密码已保存，留空则不修改"
                : "请输入密码";
            SelectProvider(settings.Provider);
            _intervalInput.Value = settings.CheckIntervalSeconds;
            _cooldownInput.Value = settings.FailureCooldownSeconds;
            _fallbackUrlTextBox.Text = settings.FallbackProbeUrl;
            _successNotificationCheckBox.Checked = settings.SuccessNotification;
            RefreshLastLogin();
            UpdateSubmittedAccountPreview();

            try
            {
                _autoStartCheckBox.Checked =
                    await _startupManager.IsEnabledAsync().ConfigureAwait(true);
            }
            catch
            {
                _autoStartCheckBox.Checked = settings.AutoStart;
            }
        }
        finally
        {
            _loading = false;
        }
    }

    public void AllowClose()
    {
        _allowClose = true;
    }

    private void BuildInterface()
    {
        TableLayoutPanel root = new()
        {
            Dock = DockStyle.Fill,
            Padding = new Padding(20),
            ColumnCount = 2,
            RowCount = 13,
            AutoScroll = true
        };
        root.ColumnStyles.Add(new ColumnStyle(SizeType.Absolute, 150));
        root.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100));
        Controls.Add(root);

        ConfigureTextBox(_accountTextBox);
        _accountTextBox.TextChanged += (_, _) => UpdateSubmittedAccountPreview();
        AddRow(root, 0, "学号或账号", _accountTextBox);

        ConfigureTextBox(_passwordTextBox);
        _passwordTextBox.UseSystemPasswordChar = true;
        AddRow(root, 1, "密码", _passwordTextBox);

        _providerComboBox.DropDownStyle = ComboBoxStyle.DropDownList;
        _providerComboBox.Dock = DockStyle.Fill;
        foreach (Provider provider in Enum.GetValues<Provider>())
        {
            _providerComboBox.Items.Add(
                new ProviderOption(provider, provider.GetDisplayName()));
        }
        _providerComboBox.SelectedIndexChanged += (_, _) =>
            UpdateSubmittedAccountPreview();
        AddRow(root, 2, "服务商", _providerComboBox);

        _submittedAccountLabel.Dock = DockStyle.Fill;
        _submittedAccountLabel.AutoEllipsis = true;
        _submittedAccountLabel.TextAlign = ContentAlignment.MiddleLeft;
        AddRow(root, 3, "实际提交账号", _submittedAccountLabel);

        ConfigureNumberInput(_intervalInput, 10, 3600, 30);
        AddRow(root, 4, "检测间隔（秒）", _intervalInput);

        ConfigureNumberInput(_cooldownInput, -1, 3600, 1);
        FlowLayoutPanel cooldownPanel = new()
        {
            Dock = DockStyle.Fill,
            AutoSize = true,
            FlowDirection = FlowDirection.LeftToRight,
            WrapContents = true,
            Margin = Padding.Empty
        };
        Label cooldownHint = new()
        {
            AutoSize = true,
            ForeColor = SystemColors.GrayText,
            Text = "-1：失败后暂停自动重连；0：不额外冷却",
            Margin = new Padding(10, 8, 0, 0)
        };
        cooldownPanel.Controls.Add(_cooldownInput);
        cooldownPanel.Controls.Add(cooldownHint);
        AddRow(root, 5, "失败冷却（秒）", cooldownPanel);

        ConfigureTextBox(_fallbackUrlTextBox);
        AddRow(root, 6, "国内备用探测", _fallbackUrlTextBox);

        _autoStartCheckBox.Text = "当前用户登录 Windows 后自动启动";
        _autoStartCheckBox.AutoSize = true;
        AddRow(root, 7, "开机自启", _autoStartCheckBox);

        _successNotificationCheckBox.Text = "自动登录成功时显示系统通知";
        _successNotificationCheckBox.AutoSize = true;
        AddRow(root, 8, "成功通知", _successNotificationCheckBox);

        _lastLoginLabel.TextAlign = ContentAlignment.MiddleLeft;
        _lastLoginLabel.Dock = DockStyle.Fill;
        AddRow(root, 9, "最近成功登录", _lastLoginLabel);

        _networkStatusLabel.Text = "等待检测";
        _networkStatusLabel.TextAlign = ContentAlignment.MiddleLeft;
        _networkStatusLabel.Dock = DockStyle.Fill;
        _networkStatusLabel.AutoEllipsis = true;
        AddRow(root, 10, "当前网络状态", _networkStatusLabel);

        Label warningLabel = new()
        {
            AutoSize = true,
            MaximumSize = new Size(470, 0),
            ForeColor = Color.DarkGoldenrod,
            Text = "密码使用 Windows 当前用户身份加密保存；校园网登录接口为 HTTP，传输安全性由认证服务器决定。",
            Margin = new Padding(3, 12, 3, 12)
        };
        root.Controls.Add(warningLabel, 1, 11);

        FlowLayoutPanel buttons = new()
        {
            Dock = DockStyle.Fill,
            AutoSize = true,
            FlowDirection = FlowDirection.RightToLeft,
            WrapContents = true,
            Padding = new Padding(0, 12, 0, 0)
        };

        _saveButton.Text = "保存";
        _saveButton.AutoSize = true;
        _saveButton.Click += async (_, _) => await SaveAsync();

        Button closeButton = new()
        {
            Text = "关闭",
            AutoSize = true
        };
        closeButton.Click += (_, _) => Hide();

        Button clearButton = new()
        {
            Text = "⚠ 清除所有数据并退出",
            AutoSize = true,
            ForeColor = Color.White,
            BackColor = Color.Firebrick,
            FlatStyle = FlatStyle.Flat
        };
        clearButton.Click += async (_, _) => await ClearAllDataAsync();

        buttons.Controls.Add(_saveButton);
        buttons.Controls.Add(closeButton);
        buttons.Controls.Add(clearButton);
        root.Controls.Add(buttons, 0, 12);
        root.SetColumnSpan(buttons, 2);

        for (int row = 0; row < 13; row++)
        {
            root.RowStyles.Add(new RowStyle(SizeType.AutoSize));
        }
        AcceptButton = _saveButton;
    }

    private static void ConfigureTextBox(TextBox textBox)
    {
        textBox.Dock = DockStyle.Fill;
        textBox.Margin = new Padding(3, 5, 3, 5);
    }

    private static void ConfigureNumberInput(
        NumericUpDown input,
        int minimum,
        int maximum,
        int increment)
    {
        input.Minimum = minimum;
        input.Maximum = maximum;
        input.Increment = increment;
        input.ThousandsSeparator = true;
        input.Dock = DockStyle.Left;
        input.Width = 160;
    }

    private static void AddRow(
        TableLayoutPanel panel,
        int row,
        string labelText,
        Control control)
    {
        Label label = new()
        {
            Text = labelText,
            AutoSize = true,
            Anchor = AnchorStyles.Left,
            Margin = new Padding(3, 8, 3, 8)
        };
        panel.Controls.Add(label, 0, row);
        panel.Controls.Add(control, 1, row);
    }

    private async Task SaveAsync()
    {
        _saveButton.Enabled = false;
        try
        {
            Provider provider = SelectedProvider();
            string encryptedPassword = _settingsManager.Current.EncryptedPassword;
            if (!string.IsNullOrWhiteSpace(_passwordTextBox.Text))
            {
                encryptedPassword = _credentialProtector.Encrypt(_passwordTextBox.Text);
            }

            AppSettings candidate = _settingsManager.Current with
            {
                Account = _accountTextBox.Text.Trim(),
                Provider = provider,
                EncryptedPassword = encryptedPassword,
                CheckIntervalSeconds = Decimal.ToInt32(_intervalInput.Value),
                FailureCooldownSeconds = Decimal.ToInt32(_cooldownInput.Value),
                FallbackProbeUrl = _fallbackUrlTextBox.Text.Trim(),
                AutoStart = _autoStartCheckBox.Checked,
                SuccessNotification = _successNotificationCheckBox.Checked
            };

            IReadOnlyList<string> errors = candidate.Validate(requirePassword: true);
            if (errors.Count > 0)
            {
                MessageBox.Show(
                    this,
                    string.Join(Environment.NewLine, errors),
                    "无法保存设置",
                    MessageBoxButtons.OK,
                    MessageBoxIcon.Warning);
                return;
            }

            if (_autoStartCheckBox.Checked)
            {
                await _startupManager.EnableAsync().ConfigureAwait(true);
            }
            else
            {
                await _startupManager.DisableAsync().ConfigureAwait(true);
            }

            await _settingsManager.UpdateAsync(current => candidate with
            {
                LastSuccessfulLoginUtc = current.LastSuccessfulLoginUtc
            }).ConfigureAwait(true);

            _passwordTextBox.Clear();
            _passwordTextBox.PlaceholderText = "密码已保存，留空则不修改";
            await _logger.LogAsync("settings-changed", "用户保存了设置。")
                .ConfigureAwait(true);
            _settingsSaved();

            MessageBox.Show(
                this,
                "设置已保存。",
                "校园网自动登录",
                MessageBoxButtons.OK,
                MessageBoxIcon.Information);
        }
        catch (Exception exception)
        {
            MessageBox.Show(
                this,
                "保存失败：" + exception.Message,
                "校园网自动登录",
                MessageBoxButtons.OK,
                MessageBoxIcon.Error);
        }
        finally
        {
            _saveButton.Enabled = true;
        }
    }

    private async Task ClearAllDataAsync()
    {
        const string details =
            "将删除：\n\n" +
            "• 已保存账号和加密密码\n" +
            "• 配置、日志和最近登录时间\n" +
            "• Windows 开机自启任务\n\n" +
            "程序随后退出，但不会删除程序 EXE。";

        if (MessageBox.Show(
                this,
                details,
                "确认清除所有数据？",
                MessageBoxButtons.YesNo,
                MessageBoxIcon.Warning,
                MessageBoxDefaultButton.Button2) != DialogResult.Yes)
        {
            return;
        }

        if (MessageBox.Show(
                this,
                "这是最后一次确认。清除后无法恢复，是否继续？",
                "再次确认",
                MessageBoxButtons.YesNo,
                MessageBoxIcon.Stop,
                MessageBoxDefaultButton.Button2) != DialogResult.Yes)
        {
            return;
        }

        Enabled = false;
        try
        {
            CleanupResult result = await _cleanup().ConfigureAwait(true);
            if (!result.Succeeded)
            {
                string failed = result.FailedPaths.Count == 0
                    ? string.Empty
                    : Environment.NewLine + string.Join(Environment.NewLine, result.FailedPaths);
                MessageBox.Show(
                    this,
                    (result.Error ?? "清除失败。") + failed,
                    "清除未完成",
                    MessageBoxButtons.OK,
                    MessageBoxIcon.Error);
                return;
            }

            string cacheMessage = result.RemainingBundleCachePaths.Count == 0
                ? string.Empty
                : "\n\n当前运行时缓存可能仍被占用。程序退出后可手动删除：\n" +
                  string.Join(Environment.NewLine, result.RemainingBundleCachePaths);

            MessageBox.Show(
                this,
                "应用数据和开机任务已清除，程序即将退出。" +
                cacheMessage +
                "\n\n退出后可删除 CampusNetAutoLogin.exe。",
                "清除完成",
                MessageBoxButtons.OK,
                MessageBoxIcon.Information);
            _exitAfterCleanup();
        }
        finally
        {
            if (!IsDisposed)
            {
                Enabled = true;
            }
        }
    }

    private Provider SelectedProvider() =>
        _providerComboBox.SelectedItem is ProviderOption option
            ? option.Value
            : Provider.Campus;

    private void SelectProvider(Provider provider)
    {
        for (int index = 0; index < _providerComboBox.Items.Count; index++)
        {
            if (_providerComboBox.Items[index] is ProviderOption option &&
                option.Value == provider)
            {
                _providerComboBox.SelectedIndex = index;
                return;
            }
        }

        _providerComboBox.SelectedIndex = 0;
    }

    private void UpdateSubmittedAccountPreview()
    {
        string account = _accountTextBox.Text.Trim();
        _submittedAccountLabel.Text = string.IsNullOrWhiteSpace(account)
            ? "—"
            : account + SelectedProvider().GetSuffix();
    }

    private void HandleFormClosing(object? sender, FormClosingEventArgs e)
    {
        if (!_allowClose && e.CloseReason == CloseReason.UserClosing)
        {
            e.Cancel = true;
            Hide();
        }
    }

    private sealed record ProviderOption(Provider Value, string DisplayName)
    {
        public override string ToString() => DisplayName;
    }
}
