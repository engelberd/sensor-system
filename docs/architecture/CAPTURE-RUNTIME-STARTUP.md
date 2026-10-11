# Capture runtime startup

- Status: parser-independent composition, profil instalacji i grouped executor
  zaimplementowane oraz przetestowane na rzeczywistym filesystemie
- Ostatnia aktualizacja: 2026-10-10

## Cel

Start hosta nie może zgadywać durable cursora z konfiguracji ani tworzyć
`NodeSession` przed recovery plików. `CaptureRuntimeConfig` definiuje małą
granicę pomiędzy konfiguracją instalacji a krytyczną logiką storage.

Kolejność startu jest stała:

1. profile instalacji dostarczają topologię, ścieżki i polityki;
2. zweryfikowany discovery/handshake dostarcza `hardware_id` i bieżący
   `boot_epoch`, z których composition tworzy `CaptureStreamSpec`;
3. `CaptureRuntimeConfig` sprawdza obecność strumieni, dodatnią pojemność kolejki,
   jawną `GroupCommitPolicy` i nazwę workera;
4. `CaptureRepository::open` wykonuje recovery wszystkich skonfigurowanych WAL;
5. runtime kopiuje autorytatywne `(hardware_id, boot_epoch, durable_cursor)`;
6. repozytorium przechodzi na wyłączną własność
   `GroupedPersistenceExecutor`;
7. dopiero recovered snapshot może posłużyć do budowy sesji node.

`CaptureInstallationProfile` przyjmuje statyczną politykę storage: root,
przybliżone okno, pojemność kolejki, group commit i prefiks workera. Dopiero
wynik discovery dostarcza `hardware_id` i `boot_epoch`. Profil tworzy osobny
katalog `channel/hardware/boot-epoch`, odrzuca path-like nazwy kanałów, zerowy
boot epoch oraz duplikaty przed otwarciem filesystemu.

Konfiguracja nie przesuwa durable cursora. `recovered_cursor` w specyfikacji WAL
opisuje początek/recovery contract danego segmentu, a stan znaleziony w pliku
pozostaje źródłem prawdy dla już zapisanych rekordów.

## Granice odpowiedzialności

`CaptureRuntimeConfig` nie czyta JSON, TOML ani zmiennych środowiskowych. Parser
i profile instalacji należą do zewnętrznej warstwy configuration. Dzięki temu
testy recovery, ownership i shutdown nie zależą od formatu pliku ustawień.

Pojemność kolejki oraz `GroupCommitPolicy(max_records, max_wait)` są obowiązkowe,
ale ich wartości nie zostały zamrożone. Zostaną dobrane z pomiarów ODR, liczby
node, najgorszego latency zapisu oraz docelowego Raspberry Pi z M.2 SSD.

## Zweryfikowane przypadki

Testy obejmują:

- odrzucenie pustej listy strumieni, zerowej kolejki i pustej nazwy workera;
- recovery przed przeniesieniem repozytorium na wątek;
- grupowy zapis dwóch kolejnych rekordów przez executor do rzeczywistego WAL;
- kontrolowany shutdown i zwrot repozytorium;
- ponowny start oraz odtworzenie durable cursora z filesystemu.

## Pozostałe prace

`HostInstallationProfile` ładuje tracked JSON jeden raz, rozwiązuje względne
`extends` i składa osobne profile topologii oraz storage. Warstwa storage scala
obiekty oraz tablice `channels`/`nodes` po ich stabilnej tożsamości i buduje
gotowy `CaptureInstallationProfile` z sekcji `storage.capture_v2`.
Ścieżki względne kotwiczy w jawnie przekazanym katalogu repozytorium, a literówki
w `capture_v2`, cykle dziedziczenia, zera i overflow odrzuca fail-closed.

Typowany plan discovery i walidacja kompletnego inventory są gotowe. Powiązanie
recovered snapshot z tym inventory oraz kontrolowany shutdown wszystkich
kanałów należą do przyszłego `HostCoordinator`. On rozstrzygnie również, czy
kanały współdzielą jeden executor, czy małą pulę workerów.

Wartości `64 records`, `8 records/group` i `5 ms` w `common.json` są startowym
profilem integracyjnym. Nie stanowią gwarancji pojemności; benchmark na
docelowym Raspberry Pi z M.2 może je zmienić bez naruszania domeny ani formatu
Capture.

## Niejednoznaczny błąd zapisu

Po błędzie append lub `sync_data` ponownie otwarty WAL może kończyć się przed
rekordem albo już zawierać cały rekord. `MeasurementCoordinator` zachowuje dla
`WriteJobId` zakres `[durable_before, expected_end)`, a recovered snapshot jest
dowodem rozstrzygającym:

- cursor równy początkowi grupy → ponowienie całej grupy;
- cursor na granicy rekordów → potwierdzenie pełnego prefiksu i ponowienie
  suffixu;
- cursor równy lub większy od końca grupy → potwierdzenie całej grupy bez
  ponownego appendu;
- cursor wewnątrz rekordu albo inna identity/boot epoch → fail-closed.

Operator ani timeout nie wybiera tej decyzji ręcznie. Musi ona wynikać z
recovery WAL i `classify_group_recovery`.
