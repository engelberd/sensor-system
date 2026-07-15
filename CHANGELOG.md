# Changelog

Wersjonowanie projektu stosuje format `MAJOR.MINOR.PATCH`. Numer wydania jest
wspólny dla narzędzi hosta i firmware'u.

## [0.3.10] - 2026-07-15

### Dodano

- Alarm dla czujnika, który odpowiada, ale przestał dostarczać próbki, wraz z
  komunikatem o odzyskaniu przepływu.
- Liczniki węzłów odbierających próbki w dashboardzie i rozszerzony endpoint
  health.
- Kontrolowany restart firmware'u pojedynczego czujnika z dashboardu.
- Skrypt przygotowujący środowisko hosta oraz pełną kontrolę release'u.

### Poprawiono

- Recorder jest ponownie uruchamiany także wtedy, gdy restart węzła się nie uda.
- Testy C++ firmware'u można uruchamiać niezależnie od bieżącego katalogu.
- Ujednolicono numery wersji firmware'u, recordera, supervisora, dashboardu i
  panelu operatora.
- Konfiguracja konkretnej instalacji i lokalne unity systemd nie są częścią
  dystrybucji produktu.

### Znane ograniczenia

- Dashboard i panel operatora nie mają wbudowanego uwierzytelniania; powinny być
  dostępne wyłącznie w zaufanej, chronionej sieci.
- Długotrwała praca nadal wymaga decyzji dotyczącej liczników 32-bitowych i
  jawnej obsługi zawinięcia `uptime_ms` po około 49,7 dnia.
- Diagnostyka elektryczna linii E i przyczyna potwierdzonych utraconych próbek na
  linii G pozostają zadaniami sprzętowymi przed ostatecznym odbiorem instalacji.

## [0.3.9] - 2026-07-11

- Trwała diagnostyka utraty próbek i alarmy po stronie hosta.

## [0.3.8] - 2026-07-10

- Wzmocniona obsługa FIFO i rozszerzona telemetria firmware'u.

## [0.3.0] - 2026-05-24

- Pierwszy pakowany release firmware'u z obrazem fabrycznym i aktualizacją A/B.
