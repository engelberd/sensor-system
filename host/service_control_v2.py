#!/usr/bin/env python3
from __future__ import annotations

import argparse

try:
    from host.dashboard.v2_service_control import (
        DEFAULT_SERVICE_CONTROL_SOCKET,
        ServiceControlServer,
        SystemdServiceController,
    )
except ModuleNotFoundError:
    from v2_service_control import (  # type: ignore[no-redef]
        DEFAULT_SERVICE_CONTROL_SOCKET,
        ServiceControlServer,
        SystemdServiceController,
    )


def main() -> int:
    parser = argparse.ArgumentParser(description="Bounded systemd control helper for Sensor System host v2")
    parser.add_argument("--socket", default=str(DEFAULT_SERVICE_CONTROL_SOCKET))
    parser.add_argument("--host-unit", default="sensor-host-v2.service")
    parser.add_argument("--analysis-timer", default="sensor-analysis-export-v2.timer")
    args = parser.parse_args()
    ServiceControlServer(
        args.socket,
        SystemdServiceController(args.host_unit, args.analysis_timer),
    ).serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
