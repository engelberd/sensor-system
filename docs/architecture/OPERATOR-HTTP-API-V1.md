# Operator HTTP API v1

- Status: Configuration UI i sterowanie usługą zaimplementowane lokalnie
- Zakres: cienki adapter dashboardu do lokalnego Operations API hosta v2

## Granica odpowiedzialności

HTTP nie jest drugim control plane. Dashboard nie otwiera RS485, nie posiada
stanu kanału i nie wykonuje logiki lifecycle. Każde polecenie jest przekazywane
jako ograniczony datagram schema 1 do
`/run/sensor-system/operations-v1.sock`. Jedynym właścicielem kanału pozostaje
worker `sensor-host`.

Złożenie operacji jest asynchroniczne. Odpowiedź `202 Accepted` zawiera
`operation_id`; klient odpytuje ten sam worker-owned ledger aż do wyniku
`succeeded`, `rejected` albo `failed`. Ponowienie identycznego requestu z tym
samym ID zachowuje idempotency Operations API.

## Endpointy

| Metoda | Ścieżka | Znaczenie |
| --- | --- | --- |
| GET | `/api/v2/operator/capabilities` | katalog operacji i granic adaptera |
| GET | `/api/v2/service` | stan recordera i timera eksportu |
| POST | `/api/v2/service/{start|stop|restart}` | sterowanie całym hostem przez allowlisted helper |
| GET | `/api/v2/channels/{channel_id}/operations/status` | tryb kanału i aktywna operacja |
| POST | `/api/v2/channels/{channel_id}/operations` | złożenie jednej operacji |
| GET | `/api/v2/channels/{channel_id}/operations/{operation_id}` | odczyt fazy i wyniku |

Mutujący POST wymaga `Content-Type: application/json` oraz nagłówka
`X-Sensor-System-Request: dashboard-v2`. Własny nagłówek blokuje prosty
cross-origin form POST. Dashboard nadal jest dostępny wyłącznie w prywatnej
sieci administracyjnej; nagłówek nie zastępuje autoryzacji sieciowej.

Przykład:

```json
{
  "operation": "restart_node",
  "node_address": 1,
  "operation_id": 123456
}
```

Jeśli `operation_id` nie zostanie podane, adapter generuje niezerowe `u64` i
zwraca je klientowi. Limit requestu i odpowiedzi pozostaje zgodny z IPC i
wynosi 4096 bajtów.

## Dostępne operacje

Kanał:

- `enter_maintenance`;
- `resume_recording`;
- `scan_unassigned` z `round_nonce` i `slot_count`;
- `assign_address` z nonce, hardware ID i nowym adresem.

Node:

- `get_configuration`;
- `validate_configuration`;
- `apply_configuration` z `expected_revision`;
- `reset_configuration` z `expected_revision`;
- `restart_node`.

Walidacja trybu, targetu, konfiguracji, revision, admission i commissioning
pozostaje w domenie hosta. Adapter HTTP jedynie ogranicza nazwy i wymagane pola,
a następnie przekazuje request do tego samego API, z którego korzysta
`hostctl channel`.

Operacje Configuration złożone podczas `Recording` są jedną operacją domenową.
Worker wykonuje `EnteringMaintenance → Configuring → ResumingRecording`, a wynik
publikuje dopiero po pełnym Resume. Kanał, który już był w `Maintenance`, nie
jest automatycznie uruchamiany. Błąd konfiguracji pozostawia stabilne
`Maintenance`, `Faulted` albo `Offline`; nigdy nie jest maskowany samym Resume.
Formularz udostępnia ODR, zakres, high-pass, profil filtra, FIFO watermark,
offsety i revision. ODR nierozgłaszane przez capabilities danego node są
wyłączone w UI.

Commissioning oznacza całą procedurę przyjęcia nowego urządzenia: wyłączne okno
Maintenance, slotowany `ScanUnassigned`, wybór po `hardware_id`,
`AssignAddress`, handshake pod nowym adresem i zatwierdzenie tożsamości w
profilu instalacji. Sam skan jest tylko discovery. Wire Scan/Assign jest gotowy;
trwała aktualizacja profilu oraz panel zatwierdzania pozostają otwartą bramką.

## Osobna granica systemowa

Start, stop i restart całego `sensor-host-v2.service` nie są operacjami
działającego hosta. Obsługuje je osobny root-owned
`sensor-service-control-v2.service` przez socket mode `0660`. Helper przyjmuje
wyłącznie `status/start/stop/restart`, ma stałe nazwy jednostek i nigdy nie
buduje polecenia shell z danych HTTP. Stop wyłącza najpierw timer eksportu, a
następnie czeka na kontrolowany shutdown recordera; Start i Restart uruchamiają
timer po hostcie. Checksummowana kopia helpera jest częścią release w `/opt`,
więc root nie wykonuje kodu zapisywalnego przez użytkownika dashboardu.

`doctor` nadal pozostaje poza tym helperem. Jest read-only, lecz wymaga osobnej
polityki czasu wykonania i publikacji wyniku zamiast blokowania żądania HTTP.

## Pierwsze sterowanie dashboardu

Dashboard odczytuje tryb przez Operations IPC i pokazuje tylko operacje
dozwolone w danym stanie:

- `Recording` — przejście do Maintenance;
- `Maintenance` — Resume Recording oraz restart node;
- `Faulted` — restart node;
- stany przejściowe — aktywne operation ID bez drugiej mutacji.

Przeglądarka po submit odpytuje wynik co 250 ms przez maksymalnie 120 s.
Odświeżenie lub zamknięcie strony nie anuluje operacji; wynik nadal pozostaje w
bounded ledgerze hosta i trwałym audycie.

Następny przyrost UI obejmuje pełny commissioning wraz z zatwierdzeniem zmiany
profilu. Osobno pozostają `doctor`, operacje grupowe i uwierzytelnienie aplikacji
ponad obecną granicę prywatnej sieci Tailscale.
