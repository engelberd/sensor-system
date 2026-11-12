# ADR-0013: Wspólne okna kalendarzowe Capture WAL i Analysis CSV

- Status: przyjęte, wdrożone i sprawdzone HIL na rpi-sanok2
- Data: 2026-11-02

## Decyzja

Jeden zamknięty segment WAL odpowiada jednemu produktowi Analysis CSV.
Granice wyznacza Capture, a exporter odwzorowuje je bez ponownego grupowania.
Domyślny okres 600 s oznacza lokalne :00, :10, :20 itd. w IANA
`system.timezone`. Start 12:03 tworzy krótki plik do pierwszej próbki na
12:10 lub po niej. Ta próbka rozpoczyna kolejne okno. Nie przesuwamy timestampów
próbek do granicy, nie dopełniamy danych i nie tworzymy pustych plików.

Zmiana `config_revision`, również samych offsetów, zamyka poprzednie okno przed
pierwszą próbką nowej konfiguracji. Następne okno znów kończy się na najbliższej
granicy kalendarzowej. Restart i ręczne zatrzymanie mogą utworzyć krótsze okna.

CSV trafiają do `analysis_root/DD_MM_YYYY/channel/`, według dnia pierwszej
próbki. WAL pozostaje w technicznym drzewie hardware/boot. Nazwa produktu
jest czytelna dla użytkownika: `line_b-node_1-21_30-02_11_2026.csv`.
Kolejne okna tej samej minuty otrzymują `_2`, `_3` itd., również przy
powtórzonej godzinie po zmianie czasu. Hardware, boot i ordinal pozostają
w JSON i służą do rozpoznawania ponownego eksportu, niezależnie od nazwy.
Układ binarny WAL v2 i pięć kolumn CSV v3 pozostają bez zmian.

## Czas i trwałość

Przed pierwszym dopisaniem próbki storage worker zapisuje atomowo i z fsync
`window-<ordinal>.calendar.json`. Sidecar zawiera wersję, ordinal, CRC32C,
strefę, kotwicę device/UTC, deadline device/UTC i niepewność. Jest usuwany
razem z segmentem dopiero zgodnie z checkpointem retencji.

Kotwica korzysta z Time WAL, gdy dostępne są obserwacje synchronizacji.
W przeciwnym razie wykorzystuje czas odbioru pomniejszony o rozpiętość chunku;
niepewność jest wtedy nieznana (`null`). To przybliżenie, nie potwierdzenie
bezwzględnej dokładności UTC. Mapowanie przyrostu device na UTC jest zamrożone
1:1 dla całego okna; późniejsza synchronizacja nie przenosi próbek między
plikami. Cofnięcie nowego oszacowania względem poprzedniego okna jest
ograniczane do jego projekcji, z nieznaną niepewnością. Dodatnia korekta może
skrócić następne okno. Export używa zapisanej kotwicy, również po restarcie.

Chunk transferowy może zostać podzielony między rzeczywistymi próbkami.
Każdy fragment zachowuje dokładny timestamp i cursor. Oryginalny receipt jest
potwierdzany dopiero po utrwaleniu wszystkich fragmentów. Jeśli proces padnie
po trwałym prefiksie, recovery przycina oczekujący chunk do brakującego sufiksu;
nie potwierdza go przedwcześnie. Gap i ContinuityBreak zachowują kolejność i
pozycję w ledgerze, bez przypisywania im fikcyjnego czasu próbki.

## Odpowiedzialności i migracja

`capture_calendar` odpowiada za kalendarz i jego trwałą kotwicę;
`CaptureSegmentStream` za granice i fragmenty; ledger za ciągłość cursorów;
persistence executor za receipt i recovery; exporter za pochodny produkt.
Żadne dodatkowe I/O nie trafia do pętli RS485 i nie zmienia się narzut wire.

Stare zamknięte WAL pozostają czytelne. Otwarty segment bez kotwicy zostaje
zamknięty przed rozpoczęciem nowej polityki. Historyczne okna mieszające
konfiguracje nie są przepisywane; exporter odrzuca takie okno. Nie rekonstruujemy
starego podziału równolegle do nowego.

## Weryfikacja

Testy obejmują start częściowego okna, dokładną granicę i podział chunku,
ciągłość po reopen, recovery trwałego prefiksu, zmianę offsetów, Gap i Break,
północ i codzienne CSV, DST, przesunięcie strefy o 45 minut, CRC sidecara oraz
idempotencję eksportu. Weryfikację fizyczną granicy 21:30, zmiany offsetów i restartu opisuje
[raport wdrożenia](../architecture/RPI-SANOK2-CALENDAR-DEPLOYMENT-2026-11-02.md).
