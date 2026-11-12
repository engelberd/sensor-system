# ADR-0012: indywidualny czas każdej próbki

- Status: accepted
- Date: 2026-11-02
- Decyzja użytkownika: rozwijamy jeden bieżący system, bez utrzymywania starego trybu recording.

## Problem

Pico rejestruje `time_us_64()` przy obsłudze DRDY i wiąże czas z XYZ z FIFO.
FIR z decymacją przypisuje wynikowi czas środkowej próbki wejściowej.
Journal zachowuje czas każdego wyniku, lecz protocol 5 przenosił tylko dwie
kotwice i maksymalny residual. Eksport rekonstruował czasy wewnętrzne liniowo,
co uniemożliwiało analizę indywidualnych odstępów.

## Decyzja

1. Application protocol **6**, bez negocjacji v5. Framing pozostaje 3.
2. Każdy SampleChunk ma dokładnie tyle indywidualnych timestampów, co XYZ.
   `SampleTimes` w sensor-core jest ograniczonym, walidowanym typem bez wiedzy o I/O.
3. Zachowujemy czas Pico, nie czas odbioru UART ani fikcyjne `index / ODR`.
   Rozdzielczość wynosi 1 us; dokładność zależy od IRQ, zegara i toru sensora.
4. Wire zachowuje istniejący nagłówek 66 B i XYZ24, następnie bajt szerokości
   2/4 oraz N-1 dodatnich odstępów w us od poprzedniej próbki. Pierwszy czas
   jest w nagłówku. Szerokość jest wybierana według rzeczywistych odstępów.
5. Limit chunku jest konserwatywny dla u32: **34 próbki**, payload <= 512 B.
   Obie szerokości mają ten sam limit, więc capabilities i admission nie
   wymagają zależnego od treści maksymalnego rozmiaru chunku. Cel pozostaje 32.
6. Czas końcowy musi zgadzać się z sumą delt; zero, overflow, nieznana
   szerokość i niezgodna długość są błędami. Odstęp > UINT32_MAX us jest
   odrzucany, bez obcinania ani interpolacji. Nieprawidłowe powiązanie DRDY
   obsługuje istniejąca ścieżka quality events.
7. Capture WAL **v2** przechowuje indywidualne u64 LE po XYZ. Prosty zapis
   storage nie musi powtarzać kompresji wire. Każdy timestamp jest objęty CRC
   i tą samą barierą durability co XYZ. Nie zmieniamy reguły `fsync -> Release`.
8. Analysis CSV **v3** i techniczny Capture CSV **v2** eksportują czasy
   bez interpolacji. UTC pozostaje osobną estymacją Time Service.
9. Stare protokoły i pliki WAL nie są obsługiwane przez nowy recorder.
   Stare dane zachowujemy oddzielnie; nie otwieramy ich nowym writerem.
10. Wolne ODR 31.25 i 7.8125 są rzeczywiście konfigurowalne. Lokalne Hz
    konfiguracji firmware używają float dla dokładnych binarnych ułamków
    rodziny ADXL355; na wire nadal są wyłącznie kody całkowite. Jedna tabela
    `SensorRate` odpowiada za odwzorowanie rejestru na Hz. Persistent config
    przechodzi do v5; jednorazowy odczyt v4 zachowuje tożsamość i konfigurację
    instalacji, nie stanowi drugiego trybu runtime.

## Odpowiedzialności i skutki

- sterownik: DRDY i wiarygodne powiązanie z FIFO;
- filtr: czas środka FIR; journal: przechowanie źródłowego czasu;
- delivery: istniejący prepare/complete/abort; codec: u16/u32 i granice payloadu;
- host session: dotychczasowe kursory; storage: atomowy fakt XYZ+czas;
- eksporter: jednostki fizyczne i jawna oś czasu; loader: odczyt integer u64.

Nie dodajemy właściciela portu, osobnej kolejki timestampów ani interfejsów
bez potrzeby drugiej implementacji. Zapis WAL rośnie o 8 B/próbkę. Wire rośnie
o 1+2(N-1) albo 1+4(N-1) B. Domyślne budżety 51 ms exchange / 52 ms slot
obejmują konserwatywny 34-próbkowy wariant u32 przy 115200 8N1 i ~1 ms
turnaround; wymagają pomiaru na docelowym sprzęcie. Admission uwzględnia
niepełne porcje przy wolnym ODR, konserwatywnie naliczając pełny koszt
exchange zamiast zakładać zawsze 32 próbki.

## Weryfikacja i wdrożenie

Wspólne niezależne golden vectors C++/Rust dla u16 i u32, granica
65535/65536 us, jitter, wolne ODR, maksymalny payload i uszkodzone delty.
Test wire -> WAL -> replay -> CSV wymaga identycznych czasów, w tym
wewnętrznego timestampu różnego od interpolacji. Loader nie konwertuje u64
przez float. Istniejące testy retry, fault injection, crash-tail i scheduler
pozostają wymagane.

Wdrożenie: kontrolowany Stop, osobny nowy katalog Capture/Analysis, aktualizacja
hosta i firmware, walidacja board revision, doctor i HIL. Nie kontynuujemy
starych WAL-i w tym samym katalogu. HIL: 125/250 oraz 31.25/7.8125 Hz,
restart/recovery, backlog, luki, TimeSync i zgodność z DRDY. Sam build nie
potwierdza dokładności fizycznej ani wydajności nośnika.
