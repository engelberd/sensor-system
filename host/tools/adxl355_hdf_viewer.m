function adxl355_hdf_viewer(filename, node_id)
%ADXL355_HDF_VIEWER Inspect ADXL355 recordings written by host_recorder.py.
%
%   adxl355_hdf_viewer(filename)
%   adxl355_hdf_viewer(filename, node_id)
%
% The current recorder stores acceleration in m/s^2 and the effective sample
% rate in the output_odr_hz attribute.  Older files containing raw sensor
% counts are also supported when accel_unit is "counts" or "lsb".

if nargin < 2
    node_id = 1;
end

g0 = 9.80665;
group = sprintf('/nodes/%d', node_id);
dataset = sprintf('%s/samples', group);

info = h5info(filename, dataset);
sample_count = info.Dataspace.Size(1);

fprintf("File: %s\n", filename);
fprintf("Dataset: %s\n", dataset);
fprintf("Samples in dataset: %d\n", sample_count);

try
    data = h5read(filename, dataset);
catch ME
    warning("Full h5read failed: %s", ME.message);
    safe_count = max(1, sample_count - 4096);
    fprintf("Trying the first %d complete samples...\n", safe_count);
    data = h5read(filename, dataset, 1, safe_count);
end

seq = double(data.sample_seq(:));
x = double(data.x(:));
y = double(data.y(:));
z = double(data.z(:));

if numel(seq) < 16
    error("At least 16 samples are required.");
end

% The recorder's current HDF5 format stores acceleration in m/s^2.
accel_unit = lower(strtrim(char(string( ...
    readH5Attribute(filename, group, 'accel_unit', 'm/s^2')))));
range_g = double(readH5Attribute(filename, group, 'range_g', 2));

switch accel_unit
    case {'m/s^2', 'm/s2', 'm s-2'}
        x_ms2 = x;
        y_ms2 = y;
        z_ms2 = z;
        unit_note = "values read directly as m/s^2";
    case {'count', 'counts', 'lsb', 'raw'}
        scale_g_per_lsb = rawScaleForRange(range_g);
        x_ms2 = x * scale_g_per_lsb * g0;
        y_ms2 = y * scale_g_per_lsb * g0;
        z_ms2 = z * scale_g_per_lsb * g0;
        unit_note = sprintf("raw counts converted for +/-%.0fg", range_g);
    otherwise
        error("Unsupported accel_unit attribute: '%s'.", accel_unit);
end

mag_ms2 = sqrt(x_ms2.^2 + y_ms2.^2 + z_ms2.^2);

% Prefer the effective output rate saved by the recorder.  Reception time is
% not a reliable sample clock because samples are transported in packets.
fs = double(readH5Attribute(filename, group, 'output_odr_hz', NaN));
if ~isfinite(fs) || fs <= 0
    fs = double(readH5Attribute(filename, group, 'sensor_odr_hz', NaN)) / 2;
end
if ~isfinite(fs) || fs <= 0
    fs = 125;
    warning("No sample-rate metadata found; using %.3f Hz.", fs);
end

% Build a monotonic sample time. Forward sequence gaps remain visible as
% gaps in time; a reset starts a new contiguous run one sample later.
dseq = diff(seq);
sample_steps = dseq;
sample_steps(sample_steps < 1) = 1;
t = [0; cumsum(sample_steps)] / fs;

bad = find(dseq ~= 1);
missing_count = sum(max(dseq - 1, 0));
reset_count = nnz(dseq <= 0);

fprintf("\n=== RECORDING ===\n");
fprintf("Acceleration: %s\n", unit_note);
fprintf("Sample rate:  %.6f Hz\n", fs);
fprintf("Duration:     %.3f s\n", t(end));
fprintf("Seq gaps:     %d (%d missing samples, %d resets/backward jumps)\n", ...
    numel(bad), missing_count, reset_count);

fprintf("\n=== NOISE, WHOLE RECORDING ===\n");
printNoiseStats("X", x_ms2, fs, g0);
printNoiseStats("Y", y_ms2, fs, g0);
printNoiseStats("Z", z_ms2, fs, g0);
printNoiseStats("MAG", mag_ms2, fs, g0);

all_signal = [x_ms2; y_ms2; z_ms2];
y_min = min(all_signal);
y_max = max(all_signal);
y_margin = 0.05 * max(1e-12, y_max - y_min);
time_ylim = [y_min - y_margin, y_max + y_margin];

duration = max(t(end), 1 / fs);
min_window = min(0.5, duration);
max_window = max(min_window, duration);
initial_window = min(8, max_window);

fig = uifigure( ...
    'Name', 'ADXL355 HDF5 Viewer', ...
    'Position', [100 100 1300 900]);

layout = uigridlayout(fig, [6 4]);
layout.RowHeight = {30, 250, 250, 40, 195, '1x'};
layout.ColumnWidth = {'1x', '1x', '1x', '1x'};

status_label = uilabel(layout, 'Text', 'Ready', 'FontWeight', 'bold');
status_label.Layout.Row = 1;
status_label.Layout.Column = [1 4];

ax_time = uiaxes(layout);
ax_time.Layout.Row = 2;
ax_time.Layout.Column = [1 4];
plot(ax_time, t, x_ms2, 'DisplayName', 'X');
hold(ax_time, 'on');
plot(ax_time, t, y_ms2, 'DisplayName', 'Y');
plot(ax_time, t, z_ms2, 'DisplayName', 'Z');
title(ax_time, 'Time signal');
xlabel(ax_time, 'Time [s]');
ylabel(ax_time, 'Acceleration [m/s^2]');
legend(ax_time, 'show');
grid(ax_time, 'on');
xlim(ax_time, [0, duration]);
ylim(ax_time, time_ylim);

ax_fft = uiaxes(layout);
ax_fft.Layout.Row = 3;
ax_fft.Layout.Column = [1 4];
title(ax_fft, 'Single-sided amplitude spectrum');
xlabel(ax_fft, 'Frequency [Hz]');
ylabel(ax_fft, 'Peak amplitude [mg]');
grid(ax_fft, 'on');
xlim(ax_fft, [0, fs / 2]);

window_edit = uieditfield(layout, 'numeric', ...
    'Value', initial_window, ...
    'Limits', [min_window, max_window], ...
    'Tooltip', 'FFT window length in seconds');
window_edit.Layout.Row = 4;
window_edit.Layout.Column = 1;

axis_drop = uidropdown(layout, ...
    'Items', {'Z', 'X', 'Y', 'Magnitude'}, ...
    'Value', 'Z');
axis_drop.Layout.Row = 4;
axis_drop.Layout.Column = 2;

play_btn = uibutton(layout, ...
    'Text', 'Play', ...
    'ButtonPushedFcn', @(~,~) togglePlay());
play_btn.Layout.Row = 4;
play_btn.Layout.Column = 3;

noise_text = uitextarea(layout, ...
    'Editable', 'off', ...
    'FontName', 'Consolas');
noise_text.Layout.Row = 5;
noise_text.Layout.Column = [1 4];

slider = uislider(layout, ...
    'Limits', [0, max(eps, duration - initial_window)], ...
    'Value', 0);
slider.Layout.Row = 6;
slider.Layout.Column = [1 4];

timer_obj = timer( ...
    'ExecutionMode', 'fixedSpacing', ...
    'Period', 0.25, ...
    'TimerFcn', @(~,~) timerStep());
is_playing = false;

slider.ValueChangingFcn = @(~, event) updateFFT(event.Value);
slider.ValueChangedFcn = @(~, event) updateFFT(event.Value);
window_edit.ValueChangedFcn = @(~,~) updateLimitsAndFFT();
axis_drop.ValueChangedFcn = @(~,~) updateFFT(slider.Value);
fig.CloseRequestFcn = @(~,~) closeViewer();

updateLimitsAndFFT();

    function updateLimitsAndFFT()
        window_s = window_edit.Value;
        slider.Limits = [0, max(eps, duration - window_s)];
        slider.Value = min(slider.Value, slider.Limits(2));
        updateFFT(slider.Value);
    end

    function signal = selectedSignal()
        switch axis_drop.Value
            case 'X'
                signal = x_ms2;
            case 'Y'
                signal = y_ms2;
            case 'Z'
                signal = z_ms2;
            otherwise
                signal = mag_ms2;
        end
    end

    function updateFFT(start_time)
        window_s = window_edit.Value;
        candidates = find(t >= start_time & t < start_time + window_s);
        idx = longestContiguousRun(candidates, seq);

        if numel(idx) < 16
            status_label.Text = 'Selected window has fewer than 16 contiguous samples';
            return;
        end

        signal = selectedSignal();
        segment = signal(idx);
        [frequency, amplitude] = oneSidedAmplitude(segment, fs);
        amplitude(1) = 0; % do not display the removed DC component

        % Choose a readable physical unit for the current spectrum. Scaling
        % is recomputed for every axis and every window, so peaks are neither
        % clipped nor flattened by an unrelated fixed Z-axis limit.
        [display_amplitude, unit_label] = readableAmplitude(amplitude, g0);

        cla(ax_fft);
        plot(ax_fft, frequency, display_amplitude, 'LineWidth', 1.1);
        grid(ax_fft, 'on');
        xlim(ax_fft, [0, fs / 2]);

        ymax = max(display_amplitude);
        if ~isfinite(ymax) || ymax <= 0
            ymax = 1;
        end
        ylim(ax_fft, [0, 1.10 * ymax]);

        actual_t0 = t(idx(1));
        actual_t1 = t(idx(end)) + 1 / fs;
        resolution_hz = fs / numel(segment);
        title(ax_fft, sprintf( ...
            'FFT %s | %.3f-%.3f s | df = %.4g Hz', ...
            axis_drop.Value, actual_t0, actual_t1, resolution_hz));
        xlabel(ax_fft, 'Frequency [Hz]');
        ylabel(ax_fft, sprintf('Peak amplitude [%s]', unit_label));

        current_text = makeCurrentNoiseText( ...
            axis_drop.Value, segment, fs, g0, actual_t0, actual_t1);
        noise_text.Value = [makeGlobalNoiseText(fs, g0); " "; current_text];

        skipped = numel(candidates) - numel(idx);
        status_label.Text = sprintf( ...
            'Window %.3f-%.3f s | fs=%.3f Hz | N=%d | df=%.4g Hz | skipped at gaps=%d', ...
            actual_t0, actual_t1, fs, numel(idx), resolution_hz, skipped);
    end

    function togglePlay()
        is_playing = ~is_playing;
        if is_playing
            play_btn.Text = 'Pause';
            start(timer_obj);
        else
            play_btn.Text = 'Play';
            stop(timer_obj);
        end
    end

    function timerStep()
        new_value = slider.Value + 0.5;
        if new_value >= slider.Limits(2)
            new_value = slider.Limits(1);
        end
        slider.Value = new_value;
        updateFFT(new_value);
    end

    function closeViewer()
        try
            stop(timer_obj);
            delete(timer_obj);
        catch
        end
        delete(fig);
    end

    function text = makeGlobalNoiseText(fs_local, g_local)
        sx = noiseStats(x_ms2, fs_local, g_local);
        sy = noiseStats(y_ms2, fs_local, g_local);
        sz = noiseStats(z_ms2, fs_local, g_local);
        sm = noiseStats(mag_ms2, fs_local, g_local);

        text = [
            "=== WHOLE RECORDING (mean removed) ==="
            "Axis       AC RMS [mg]       P-P [mg]    equiv. density [ug/sqrt(Hz)]"
            formatNoiseRow("X", sx)
            formatNoiseRow("Y", sy)
            formatNoiseRow("Z", sz)
            formatNoiseRow("MAG", sm)
            "Density is RMS/sqrt(fs/2): equivalent white-noise density."
        ];
    end
end

function value = readH5Attribute(filename, path, name, default_value)
try
    value = h5readatt(filename, path, name);
catch
    value = default_value;
end
end

function scale = rawScaleForRange(range_g)
switch round(range_g)
    case 2
        scale = 3.9e-6;
    case 4
        scale = 7.8e-6;
    case 8
        scale = 15.6e-6;
    otherwise
        error("Unsupported ADXL355 range: +/-%.3g g.", range_g);
end
end

function idx = longestContiguousRun(candidates, seq)
if isempty(candidates)
    idx = candidates;
    return;
end

breaks = find(diff(candidates) ~= 1 | diff(seq(candidates)) ~= 1);
run_starts = [1; breaks + 1];
run_ends = [breaks; numel(candidates)];
[~, best] = max(run_ends - run_starts + 1);
idx = candidates(run_starts(best):run_ends(best));
end

function [frequency, amplitude] = oneSidedAmplitude(signal, fs)
signal = signal(:);
signal = signal - mean(signal);
n = numel(signal);
window = localHann(n);
spectrum = fft(signal .* window);

last_bin = floor(n / 2) + 1;
spectrum = spectrum(1:last_bin);
frequency = (0:last_bin-1)' * (fs / n);

% sum(window) is the coherent-gain correction. The result is peak
% amplitude: a bin-centred sine with peak A appears as A in the plot.
amplitude = abs(spectrum) / sum(window);
if mod(n, 2) == 0
    amplitude(2:end-1) = 2 * amplitude(2:end-1);
else
    amplitude(2:end) = 2 * amplitude(2:end);
end
end

function [display_amplitude, unit_label] = readableAmplitude(amplitude_ms2, g0)
peak_mg = max(amplitude_ms2) / g0 * 1e3;

if peak_mg < 0.1
    display_amplitude = amplitude_ms2 / g0 * 1e6;
    unit_label = 'ug';
elseif peak_mg < 1000
    display_amplitude = amplitude_ms2 / g0 * 1e3;
    unit_label = 'mg';
else
    display_amplitude = amplitude_ms2;
    unit_label = 'm/s^2';
end
end

function stats = noiseStats(signal, fs, g0)
signal = signal(:);
signal = signal(isfinite(signal));

stats.mean_ms2 = mean(signal);
ac = signal - stats.mean_ms2;

% Population standard deviation equals RMS after removing the mean. Keeping
% one value avoids reporting the same noise metric twice.
stats.ac_rms_ms2 = std(ac, 1);
stats.pp_ms2 = max(signal) - min(signal);
stats.mean_mg = stats.mean_ms2 / g0 * 1e3;
stats.ac_rms_mg = stats.ac_rms_ms2 / g0 * 1e3;
stats.pp_mg = stats.pp_ms2 / g0 * 1e3;
stats.density_ug_sqrt_hz = ...
    stats.ac_rms_ms2 / sqrt(fs / 2) / g0 * 1e6;
end

function printNoiseStats(name, signal, fs, g0)
stats = noiseStats(signal, fs, g0);
fprintf("%s: mean=%+.6f mg, AC RMS=%.6f mg, P-P=%.6f mg, ", ...
    name, stats.mean_mg, stats.ac_rms_mg, stats.pp_mg);
fprintf("equiv. density=%.3f ug/sqrt(Hz)\n", ...
    stats.density_ug_sqrt_hz);
end

function text = makeCurrentNoiseText(name, signal, fs, g0, t0, t1)
stats = noiseStats(signal, fs, g0);
text = [
    sprintf("=== CURRENT: %s | %.3f-%.3f s | N=%d ===", ...
        name, t0, t1, numel(signal))
    sprintf("Mean:                       %+12.6f mg", stats.mean_mg)
    sprintf("AC RMS (= population STD):   %12.6f mg", stats.ac_rms_mg)
    sprintf("Peak-to-peak:                %12.6f mg", stats.pp_mg)
    sprintf("Equivalent white density:    %12.3f ug/sqrt(Hz)", ...
        stats.density_ug_sqrt_hz)
];
end

function text = formatNoiseRow(name, stats)
text = sprintf("%-4s   %14.6f   %12.6f   %20.3f", ...
    name, stats.ac_rms_mg, stats.pp_mg, stats.density_ug_sqrt_hz);
end

function window = localHann(n)
if n <= 1
    window = ones(n, 1);
else
    k = (0:n-1)';
    window = 0.5 - 0.5 * cos(2 * pi * k / (n - 1));
end
end
