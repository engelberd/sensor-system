# Tor inklinacji z ADXL355 — studium

Data: 2026-10-22. Status: propozycja do dyskusji, nie zatwierdzony kontrakt.
Nie zmienia baseline ani bieżącej bramki kwalifikacji recordera.

## Wniosek i zakres

Trzy osie jednego ADXL355 pozwalają mierzyć dwa stopnie swobody względem
grawitacji. Nie pozwalają wyznaczyć yaw ani odróżnić przechyłu od dowolnego
stałego przyspieszenia liniowego. Pierwszy zakres funkcji powinien obejmować
statyczny i powolny przechył nieruchomo zamontowanego czujnika.

Należy osobno określić dokładność absolutną, powtarzalność zmian, szum,
pasmo/zwłokę oraz stabilność w czasie i temperaturze. Rozdzielczość ADC nie
jest dokładnością inklinometru. Wymagania liczbowe pozostają otwarte.

## Podstawa źródłowa

1. ADI, [AN-1057: Using an Accelerometer for Inclination Sensing](https://www.analog.com/en/resources/app-notes/2020/07/22/16/49/an-1057.html):
   geometria jedno-, dwu- i trójosiowa, kalibracja offsetu i skali, założenie
   dominacji grawitacji. Przyspieszenia stałe zafałszowują wynik.
2. ADI, [ADXL354/ADXL355, Rev. D](https://www.analog.com/media/en/technical-documentation/data-sheets/adxl354_adxl355.pdf):
   szum 22,5 µg/√Hz dla ±2 g, maksymalny współczynnik offsetu 0,15 mg/°C,
   filtry, temperatura i charakterystyki Allana. Parametry odnoszą się do
   określonych warunków, nie do dokładności całego toru.
3. Chris Murphy, ADI, [Accelerometer Tilt Measure Over Temperature and in the Presence of Vibration](https://www.analog.com/en/resources/analog-dialogue/raqs/raq-issue-144.html), 2017:
   przykładowy budżet ADXL355 daje około 0,005° przy 25°C po kompensacji,
   przy paśmie 6,25 Hz. Jest to oszacowanie w podanych warunkach, nie gwarancja
   dokładności instalacji ani stabilności wieloletniej.
4. Long Pham, Anthony DeSimone, ADI, [Vibration Rectification in MEMS Accelerometers](https://www.analog.com/en/resources/technical-articles/vibration-rectification-in-mems-accelerometers.html), 2017:
   drgania mogą powodować przesunięcie DC przez nieliniowość i asymetryczne
   obcinanie. Późniejszy LPF nie usuwa już powstałego offsetu.
5. Paul Perrault, Mahdi Sadeghi, ADI, [Managing Stress and Strain to Get the Best Performance in High Precision Tilt/Angle Sensing](https://www.analog.com/en/resources/technical-articles/managing-stress-and-strain-to-get-the-best-perf-in-hi-prec-tilt-angle-sensing.html), 2020:
   naprężenia PCB/obudowy i gradienty cieplne zmieniają offset; szczególnej
   uwagi wymaga oś Z. Mechanika i termika są częścią toru pomiarowego.
6. Bo Yuan, Zhifeng Tang, Pengfei Zhang, Fuzai Lv,
   [Thermal Calibration of Triaxial Accelerometer for Tilt Measurement](https://doi.org/10.3390/s23042105), Sensors 23(4), 2105, 2023
   ([opis i wyniki](https://pubmed.ncbi.nlm.nih.gov/36850700/)):
   eksperymenty ADXL355, model offsetu/skali/nieortogonalności, kalibracja
   wielopozycyjna i temperaturowa. W zakresie −40…60°C resztkowe dryfy osi
   po kompensacji wyniosły około −0,9, −1,1 i 1,8 mg. To dowód użyteczności
   kompensacji, ale nie dowód dokładności kątowej 0,001° w takim zakresie.
7. Jiaxin Zhu, Weifeng Wang, Shiping Huang, Wei Ding,
   [An Improved Calibration Technique for MEMS Accelerometer-Based Inclinometers](https://doi.org/10.3390/s20020452), Sensors 20(2), 452, 2020:
   ADXL355, kalibracja z odniesieniem optycznym oraz pomiary deformacji belki.
   Przypadek pomiarów względnych nie zastępuje pełnej kalibracji 3D i termicznej.

## Geometria i przetwarzanie

Dla przykładowej konwencji montażowej XYZ, z dodatnim Z w pozycji odniesienia:

```text
a_body = R_mount · C(T) · (a_source − b(T))
g_est  = LPF(a_body)
roll   = atan2(g_est.y, g_est.z)
pitch  = atan2(−g_est.x, hypot(g_est.y, g_est.z))
tilt   = atan2(hypot(g_est.x, g_est.y), g_est.z)
```

Znaki i definicje należy ustalić dla osi obiektu. `tilt` ma zakres 0…180°;
roll/pitch mają osobliwości reprezentacji. Nie są to trzy niezależne rotacje.
Warto zachować również jednostkowy wektor grawitacji. Zmiana kąta między
dwoma takimi wektorami nie mierzy obrotu wokół grawitacji.

`C(T)` koryguje skalę i sprzężenia osi; `b(T)` offset; `R_mount` opisuje
orientację względem obiektu. Dopasowanie elipsoidy wyłącznie z normy 1 g nie
identyfikuje dowolnej rotacji względem obiektu: potrzebne jest odniesienie
mechaniczne/kątowe. Współczynniki muszą uwzględniać zakres i już zastosowane
offsety sprzętowe, aby nie kompensować tego samego błędu dwa razy.

Kompensować XYZ, filtrować wektor, następnie liczyć kąty. Nie odejmować średniej,
nie usuwać trendu i nie stosować HPF w gałęzi inklinacji. Decymować dopiero po
odpowiednim antyaliasingu. Uśrednianie ogranicza szum biały, ale nie usuwa
dryfu, naprężeń ani VRE; czas uśredniania dobrać z odchylenia Allana.

## Miejsce w obecnej architekturze

```text
ADXL355 (DC zachowane) → FIR/decymacja node → journal → RS485 → Capture
                                                              ↓
Telemetry WAL + historia konfiguracji + profil kalibracji → Derived worker
                                                              ↓
                                   inklinacja + jakość → eksport / UI
```

To propozycja zgodna z rozdziałem Capture/Archive/Derived w baseline.
`MEASUREMENT-DATA-CONTRACT.md` już rozróżnia kody ADC od wyników FIR node.
Nie należy nazywać zachowanego Capture nieprzetworzonym ADC.

Pierwszy prototyp: Python offline w `host/runtime/python/`, czytający Capture oraz
powiązane konfiguracje, zdarzenia i temperaturę. Docelowy live worker może
działać w Rust jako odseparowany konsument danych trwale zapisanych, z bounded
kolejką, możliwością odtworzenia i bez wpływu na release oraz scheduler RS485.
UI prezentuje wynik; `ChannelAgent` nie posiada algorytmu inklinacji.

Przegląd kodu wykazał:

- `node/adxl355/adxl355_driver.cpp` pozwala włączyć HPF; warunkiem wyniku
  inklinacji musi być `high_pass_corner == 0`. Historycznego sygnału po HPF
  nie da się wiarygodnie odtworzyć do pomiaru statycznego.
- `node/processing/decimating_filter.h` ma FIR ×2; istnieje test zachowania
  stałej dla profilu Balanced w `node/tests/decimating_filter_test.cpp`.
  Kwalifikacja metrologiczna musi objąć wszystkie używane profile, pasmo
  tłumienia, kwantyzację oraz opóźnienie; sam test DC nie wystarczy.
- `host/configs/common.json` ustawia polling temperatury co 600 s;
  `TELEMETRY-SERVICE.md` przewiduje odświeżanie cache nie częściej niż co minutę.
  To punkt startu dla health, nie potwierdzona kadencja kompensacji inklinacji.

Gęstsza temperatura wymaga pomiaru dynamiki cieplnej oraz zmiany kontraktu,
cache/pollingu i admission, jeśli obecna kadencja okaże się niewystarczająca.
Szybszy polling starego cache sam nie dostarcza nowych obserwacji. Na benchu
warto zbierać zsynchronizowane XYZ i temperaturę np. co 1 s przez jawny tor
diagnostyczny; jest to propozycja eksperymentu, nie obecna capability.

## Elementy toru wymagające kwalifikacji

| Element | Propozycja / kryterium |
| --- | --- |
| Zakres | ±2 g, jeżeli szczyty wraz z grawitacją mieszczą się z marginesem; w przeciwnym razie większy zakres i nowa kwalifikacja |
| Montaż | sztywne odniesienie do obiektu, powtarzalne mocowanie, małe naprężenia PCB, odciążenie przewodu |
| Termika | ograniczyć gradienty i lokalne grzanie; kalibrować gotowy zespół po montażu |
| Kalibracja podstawowa | co najmniej ±1 g na każdą oś dla offsetów/skali; następnie wiele orientacji dla macierzy korekcji |
| Kalibracja temperatury | osobno per hardware ID, punkty stabilne oraz cykle grzanie/chłodzenie, walidacja na niezależnych danych |
| Filtr inklinacji | start eksperymentów od okien 1/10/60 s; ustalić pasmo i zwłokę według wymagań oraz Allana |
| Drgania | mierzyć RMS/widmo i clipping; eksperyment drgania on/off przy stałym kącie ujawni VRE |
| Referencja | wzorzec kąta o niepewności wyraźnie mniejszej od docelowego błędu; sam poziomy stół nie wystarczy |

Przykładowo przy małym kącie błąd poprzeczny daje `δθ ≈ δa/g` w radianach:
1 mg ≈ 0,0573°; 0,01° odpowiada około 175 µg, a 0,001° około 17,5 µg.
Z maksymalnego tempco datasheet wynika około 0,0086°/°C dla jednej osi
poprzecznej. Przy zmianie o 10°C to około 0,086° przed kompensacją.
To przeliczenia lokalne, nie pełny budżet błędu 3D.

Idealizowany szum przy 22,5 µg/√Hz i ENBW 0,1 Hz daje około 0,00041° RMS
dla pojedynczego małego kąta. Nie obejmuje dryfu ani błędów systematycznych;
ENBW nie jest tym samym co częstotliwość graniczna dowolnego filtra.

## Jakość, zapis i konfiguracja

Derived powinien zachować: hardware ID, boot epoch, zakres sekwencji i czas
okna, config revision, wersję kalibracji/algorytmu, referencję montażu i zera,
parametry filtra, wynik w stopniach, wektor grawitacji, RMS drgań, normę wektora,
temperaturę i jej wiek oraz flagi jakości. Nie nazywać odchylenia standardowego
pełną niepewnością pomiaru bez budżetu błędów.

Po luce, restarcie, zmianie konfiguracji/kalibracji albo resetowaniu filtra
rozpoczynać nowy segment i oznaczać warmup. Granica pliku Capture sama nie
powinna resetować ciągłego filtra. Nie interpolować przerw bez jawnej maski.
Oznaczać m.in. HPF enabled, brak/starość temperatury, temperaturę poza zakresem
kalibracji, ruch/drgania, clipping i brak kalibracji. Norma bliska 1 g oraz
niskie RMS nie dowodzą braku stałego przyspieszenia liniowego.

Kalibracja jest wersjonowanym artefaktem per hardware ID; orientacja montażu
i bazowe zero odnoszą się do konkretnej instalacji i okresu obowiązywania.
Zerowanie nie zastępuje kalibracji ani nie powinno usuwać rzeczywistej
grawitacji przez rejestry offsetu. Ustawienia produktu i instalacji powinny
respektować istniejący podział common/system/local; nie dodajemy tu schematu.

## Proponowana kolejność

1. Ustalić zakres kątów, ruch obiektu, temperatury, czas reakcji i wymagania
   osobno dla kąta absolutnego oraz zmian względem bazy.
2. Zweryfikować DC, zakres, offsety i historię konfiguracji rzeczywistych nagrań.
3. Zrobić replay offline, porównać filtry, policzyć Allan i dryf XYZ/T.
4. Skalibrować gotowy zespół, osobno zweryfikować montaż względem wzorca.
5. Wykonać testy temperatury, drgań, ponownego zasilania i ponownego montażu;
   ocenić błąd maksymalny, bias, powtarzalność, zwłokę i dostępność wyniku.
6. Dopiero po wynikach ustalić deklarowaną dokładność, kadencję temperatury
   i kontrakt Derived; następnie wdrożyć live worker i UI.

Żyroskop rozważyć, jeżeli wymagane jest śledzenie szybkich przechyłów podczas
ruchu. Sam żyroskop ani filtr Kalmana nie zapewniają rozdzielenia dowolnego
długotrwałego przyspieszenia i grawitacji bez dodatkowych założeń/odniesienia.
