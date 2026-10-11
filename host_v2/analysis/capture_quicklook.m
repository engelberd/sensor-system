function capture_quicklook(analysisDir)
%CAPTURE_QUICKLOOK Plot a sensor-system Capture CSV export.

arguments
    analysisDir (1,1) string
end

samplesPath = fullfile(analysisDir, "samples.csv");
eventsPath = fullfile(analysisDir, "events.csv");
metadataPath = fullfile(analysisDir, "metadata.json");

samples = readtable(samplesPath);
metadata = jsondecode(fileread(metadataPath));
deviceTimeSeconds = (double(samples.device_time_us_estimate) - ...
    double(samples.device_time_us_estimate(1))) / 1e6;

deltaSeconds = diff(double(samples.device_time_us_estimate)) / 1e6;
deltaSeconds = deltaSeconds(deltaSeconds > 0);
effectiveOdr = 1 / median(deltaSeconds);

fprintf("hardware_id: %s\n", metadata.hardware_id);
fprintf("samples: %d, gaps: %d, continuity breaks: %d\n", ...
    metadata.sample_count, metadata.missing_sample_count, ...
    metadata.continuity_break_count);
fprintf("estimated output rate: %.6f Hz\n", effectiveOdr);

figure("Name", "Capture raw axes");
tiledlayout(3, 1);
axesData = {samples.x_raw, samples.y_raw, samples.z_raw};
axesNames = {"X raw", "Y raw", "Z raw"};
for axisIndex = 1:3
    nexttile;
    plot(deviceTimeSeconds, axesData{axisIndex});
    grid on;
    ylabel(axesNames{axisIndex});
end
xlabel("Device time from first sample [s]");

if isfile(eventsPath)
    events = readtable(eventsPath, "TextType", "string");
    if ~isempty(events)
        disp("Capture events:");
        disp(events);
    end
end
end
