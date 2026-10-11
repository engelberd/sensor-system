# Audyt ODR i reprezentacji próbki

- Status: wymagania dla host v2 i przyszłego firmware node
- Date: 2026-09-29
- Źródło urządzenia: Analog Devices, ADXL354/ADXL355 Rev. D, tabela 44

## 1. Legalne ustawienia ADXL355

Dolne cztery bity rejestru `FILTER` (`0x28`) wybierają jednocześnie output data
rate oraz częstotliwość graniczną wewnętrznego LPF. Profil urządzenia nie
przyjmuje dowolnej liczby. Przyjmuje wyłącznie poniższe wartości:

| Kod | Sensor ODR | LPF -3 dB | Okres próbki |
| ---: | ---: | ---: | ---: |
| `0x0` | 4000 Hz | 1000 Hz | 250 us |
| `0x1` | 2000 Hz | 500 Hz | 500 us |
| `0x2` | 1000 Hz | 250 Hz | 1 ms |
| `0x3` | 500 Hz | 125 Hz | 2 ms |
| `0x4` | 250 Hz | 62.5 Hz | 4 ms |
| `0x5` | 125 Hz | 31.25 Hz | 8 ms |
| `0x6` | 62.5 Hz | 15.625 Hz | 16 ms |
| `0x7` | 31.25 Hz | 7.8125 Hz | 32 ms |
| `0x8` | 15.625 Hz | 3.90625 Hz | 64 ms |
| `0x9` | 7.8125 Hz | 1.953125 Hz | 128 ms |
| `0xA` | 3.90625 Hz | 0.9765625 Hz | 256 ms |

Wartości 1.953125 i 0.9765625 Hz w tej tabeli są częstotliwościami LPF, a nie
dodatkowymi ODR. `Adxl355Odr` modeluje legalne kody rejestru, natomiast ogólny
`SampleRate` przechowuje częstotliwość dokładnie jako dodatni `u64` w nHz.
Jeżeli przyszła operacja da częstotliwość niereprezentowalną dokładnie w nHz,
zwraca błąd zamiast ją zaokrąglić.

## 2. Próbka bez floatów

ADXL355 wystawia każdą oś jako 20-bitową liczbę two's-complement umieszczoną w
trzech rejestrach. Sterownik składa bity do prawostronnego `int32` i waliduje je
jako `Adxl355SampleCode` w zakresie `-524288..524287`.

Node może przekazywać próbki natywne albo wynik jawnie skonfigurowanego filtra
i decymacji. FIR z ujemnymi współczynnikami może chwilowo przekroczyć zakres
signed-20 mimo poprawnych wejść. Dlatego wynik potoku to osobny `SampleAxis`:

```text
node/source stream: signed integer in sensor-code units
wire:               signed i24, 3 bytes per axis, 9 bytes per XYZ
host/storage:        int32[3], exact value, no float conversion
```

Obecne trzy profile FIR mają konserwatywne maksymalne granice odpowiednio
675456, 801472 i 876992 kodów dla wejścia signed-20, więc mieszczą się ze
znacznym zapasem w signed-24. Encoder nadal musi wykrywać wyjście poza zakres;
ciche clampowanie fałszowałoby pomiar.

Słowo „raw” w danych oznacza źródłową wartość bez przeliczenia na `g` lub
`m/s²`. Metadata musi wskazać, czy jest to natywny kod sensora, czy wynik
konkretnego filtra i decymacji.

## 3. Problemy znalezione w firmware legacy

Audyt wykazał powiązane ograniczenia, których nie należy kopiować do node v2:

- `uint16_t odr_hz` nie reprezentuje ODR ułamkowych;
- driver i walidator konfiguracji akceptują tylko 4000, 2000, 1000, 500, 250
  i 125 Hz; nawet istniejący kod rejestru 62.5 Hz nie jest osiągalny;
- `DecimatingFilterX2` narzuca decymację ×2 zamiast deklarowanego potoku;
- output ODR jest wyprowadzany przez całkowite dzielenie sensor ODR;
- timeout obsługi FIFO jest liczony z całkowitego `odr_hz`;
- stały watermark przy niskim ODR powoduje wielosekundową latencję;
- encoder legacy saturuje wynik do signed-24 i tylko zwiększa licznik; nowy
  kontrakt wymaga jawnego quality event lub zatrzymania wadliwego strumienia;
- HPF i LPF zależą od wybranego ODR, więc sama liczba `high_pass_corner` bez
  pełnej konfiguracji sensora nie wystarcza do późniejszej interpretacji.

## 4. Reguły dla nowej implementacji

1. Konfiguracja przesyła legalny kod `Adxl355Odr`, nie zaokrąglone całkowite Hz.
2. `sensor_odr`, filtr, decimation factor i wynikowy `output_rate` są osobnymi
   polami oraz podlegają walidacji jako jedna konfiguracja.
3. Scheduler, watermark i timeout wynikają z dokładnego rate oraz pojemności
   buforów; nie zawierają tabel wyjątków dla konkretnych ODR.
4. Admission control może odrzucić poprawne ustawienie sensora, jeśli nie mieści
   się ono w budżecie RS485. Przykładowo jeden strumień XYZ przy 4000 Hz wymaga
   36 kB/s przed framingiem i nie mieści się w 115200 8N1.
5. Zmiana ODR lub potoku przetwarzania kończy bieżący segment danych. Nie wolno
   opisywać dwóch konfiguracji wspólnym nominalnym ODR.
6. Nieprawidłowy kod rejestru, zero rate, overflow obliczeń i niedokładna
   operacja muszą zwrócić błąd zamiast wartości domyślnej.
7. Capabilities ogłasza output ODR po całym obowiązkowym potoku node. Sensor ODR,
   decymacja i aktywna konfiguracja są osobnymi danymi Configuration Service.
   Obecna maska `0x007e` odpowiada dokładnie output
   `2000, 1000, 500, 250, 125, 62.5 Hz`.

Źródło: [ADXL354/ADXL355 Data Sheet Rev. D](https://www.analog.com/media/en/technical-documentation/data-sheets/adxl354_adxl355.pdf).
