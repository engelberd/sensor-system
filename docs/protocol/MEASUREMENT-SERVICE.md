# Measurement Service — kontrakt transferu próbek

- Status: obowiązująca semantyka; wire zamrożony w `MEASUREMENT-WIRE-V5.md`
- Date: 2026-09-29
- Application protocol: 5

## 1. Cel

Measurement Service przenosi ciągły strumień próbek z ograniczonego journala
node do crash-safe storage hosta. Zapewnia retransmisję po błędzie oraz nie
pozwala zwolnić danych, których host nie zapisał trwale.

Normalna praca używa jednej pary request/response:

```text
host -> node : PULL_SAMPLES
node -> host : SAMPLE_CHUNK albo jawny wynik bez danych
```

Node nie rozpoczyna samodzielnego burst i nie nadaje drugiej ramki bez kolejnego
requestu hosta.

## 2. Cursor jest granicą wyłączną

`SequenceCursor(N)` opisuje ciągły zakres `[0, N)`. Pierwsza próbka poza
zakresem ma `sample_seq = N`.

```text
cursor = 0    nic nie objęto; następna próbka ma seq 0
cursor = 32   objęto 0..31; następna próbka ma seq 32
cursor = 96   objęto 0..95; następna próbka ma seq 96
```

Ta reprezentacja nie potrzebuje sentinela oznaczającego „brak”. Usuwa też
niejednoznaczność, czy `up_to` obejmuje podaną wartość.

Host utrzymuje trzy monotoniczne cursory:

```text
released <= durable <= received
```

- `received`: koniec największego ciągłego zakresu rozliczonego próbkami albo
  potwierdzonym gapem;
- `durable`: koniec największego ciągłego zakresu, którego próbki albo dowód
  gapu zapisano crash-safe;
- `released`: koniec zakresu, który node przyjął jako możliwy do usunięcia.

Próba przesunięcia któregokolwiek cursora wstecz jest błędem. `durable` nie może
wyprzedzić `received`, a `released` nie może wyprzedzić `durable`. Gap przesuwa
cursor dopiero po trwałym zapisaniu jego dokładnego zakresu i przyczyny.

## 3. Tożsamość strumienia

Handshaked node ma niezmienne `hardware_id`. Każdy start aplikacji ma nowy
`boot_epoch`. Para `(hardware_id, boot_epoch)` identyfikuje ciągłość jednego
uruchomienia. Start, stop ani recovery acquisition nie zmieniają `boot_epoch`;
ewentualne przerwy są jawnie reprezentowane jako gap/event. Jeżeli w przyszłości
potrzebny będzie osobny reset sekwencji acquisition, dodamy wersjonowany
`stream_epoch`, zamiast zmieniać znaczenie istniejącego pola.

Na adresowanej magistrali request i response przenoszą tylko `boot_epoch u64`.
Powiązanie adresu z `hardware_id` pochodzi z handshake. Pozwala to odrzucić
spóźnioną odpowiedź lub release należący do poprzedniego uruchomienia bez
powtarzania hardware ID w każdej ramce.

## 4. GET_STREAM_STATE

Odpowiedź zawiera co najmniej:

| Pole | Znaczenie |
| --- | --- |
| `boot_epoch` | aktualna tożsamość uruchomienia |
| `retained_from` | początek najstarszego zakresu możliwego do retransmisji |
| `produced` | cursor za najnowszą wyprodukowaną próbką |
| `released` | cursor przyjętej zgody na zwolnienie |
| `capacity_samples` | fizyczna pojemność journala |
| `overwritten_samples` | liczba próbek utraconych przez przepełnienie |

Musi zachodzić `retained_from <= produced` i `released <= produced`.

## 5. PULL_SAMPLES

Semantyczny request:

| Pole | Typ | Bajty | Znaczenie |
| --- | --- | ---: | --- |
| `boot_epoch` | `u64` | 8 | oczekiwany strumień |
| `from` | `u64` cursor | 8 | pierwsza żądana sekwencja |
| `release_before` | `u64` cursor | 8 | trwale zapisany prefiks możliwy do usunięcia |
| `max_samples` | `u16` | 2 | górny limit odpowiedzi |

Payload requestu ma 26 bajtów. `max_samples` musi być większe od zera, a
`release_before` nie może wyprzedzić `from`.

Node najpierw waliduje `boot_epoch` i release. Nie może zastosować release z
innego uruchomienia ani wykraczającego poza zakres dopuszczony przez stan
delivery.

Zwykle release nie może wyprzedzić zakresu wcześniej dostarczonego przez node.
Wyjątkiem jest `RANGE_UNAVAILABLE`: po trwałym zapisaniu dokładnego gapu host
może przesunąć `release_before` do potwierdzonego `retained_from`, ponieważ node
już nadpisał ten zakres. Nie wolno przesunąć go dalej bez otrzymania i trwałego
zapisania kolejnych próbek albo kolejnego dowodu luki.

Nagłówek poprawnej odpowiedzi z próbkami:

| Pole | Typ | Bajty | Znaczenie |
| --- | --- | ---: | --- |
| `boot_epoch` | `u64` | 8 | strumień odpowiedzi |
| `retained_from` | `u64` cursor | 8 | obecny początek retencji |
| `produced` | `u64` cursor | 8 | obecny koniec produkcji |
| `first` | `u64` cursor | 8 | pierwsza zwracana próbka |
| `sample_count` | `u16` | 2 | liczba XYZ |

Pola Measurement mają 34 bajty. Poprawny response poprzedza je wspólnym
jednobajtowym `status = OK`, więc nagłówek payloadu wire ma 35 bajtów.
`more_available` nie wymaga pola wire: zachodzi wtedy,
gdy `first + sample_count < produced`.

Host przyjmuje odpowiedź tylko wtedy, gdy:

- `boot_epoch` jest zgodny z requestem;
- `first == request.from`;
- `sample_count > 0`; brak danych używa jawnego `NO_DATA`;
- `sample_count <= request.max_samples`;
- koniec zakresu nie przekracza `produced`;
- request nie wskazuje zakresu starszego niż `retained_from`;
- dodawanie sekwencji nie powoduje overflow.

Odpowiedź zaczynająca się po żądanym `from` nie jest automatycznie gapem. Host
ją odrzuca i ponawia request od brakującej próbki.

## 6. Jawne wyniki bez danych

Poza `SAMPLE_CHUNK` protokół potrzebuje rozróżnić:

| Wynik | Znaczenie | Działanie hosta |
| --- | --- | --- |
| `NO_DATA` | `from == produced`; node jeszcze nic nowszego nie ma | zaplanuj późniejszy pull |
| `RANGE_UNAVAILABLE` | `from < retained_from`; journal naprawdę utracił zakres | utrwal dokładny gap, potem uzgodnij cursor |
| `GAP` | `from` wskazuje zachowany dokładny brak wewnątrz retencji | utrwal zakres i przyczynę, potem przesuń cursor |
| `CONTINUITY_BREAK` | `from` wskazuje dowód nieciągłości o nieznanym rozmiarze | utrwal break i rozpocznij nowy segment |
| `STREAM_CHANGED` | `boot_epoch` nie jest aktualny | przerwij sesję i wykonaj recovery/handshake |
| `INVALID_RELEASE` | release narusza stan delivery | nie przesuwaj cursora; uzgodnij state |
| `BUSY` | node chwilowo nie może obsłużyć pull | bounded retry zgodnie ze schedulerem |

`RANGE_UNAVAILABLE` jest dowodem rzeczywistej utraty dopiero po poprawnej ramce
z aktualnym `boot_epoch` i stanem retencji. Brak odpowiedzi albo CRC error nie
jest takim dowodem.

`GAP` różni się od `RANGE_UNAVAILABLE`: node nadal posiada dokładny opis
zdarzenia i może go retransmitować. Host przyjmuje `GAP` wyłącznie, gdy zaczyna
się dokładnie w żądanym `from`; odpowiedź przeskakująca cursor jest błędem
kontraktu, a nie automatycznym dowodem utraty.

`CONTINUITY_BREAK` również zaczyna się dokładnie w `request.from`, ale nie
zawiera `missing_count`. Jego jednopozycyjny cursor span jest znacznikiem do
retencji i durable commit, a nie liczbą utraconych próbek.

## 7. RELEASE_SAMPLES

Normalna pętla przekazuje `release_before` w kolejnym `PULL_SAMPLES`, więc nie
potrzebuje osobnego potwierdzenia po każdym chunku.

`RELEASE_SAMPLES(boot_epoch, release_before)` służy do:

- spokojnego zatrzymania po ostatnim trwałym zapisie;
- zwolnienia danych przed dłuższym okresem bez pull;
- jawnego uzgodnienia cursora podczas recovery.

Operacja jest idempotentna. Powtórzenie tej samej wartości nie zmienia stanu.

## 8. Koszt

Przy framingu kandydackim request o payloadzie 26 B zajmuje około 42 B na
przewodzie, czyli około 3.6 ms przy 115200 8N1.

Dla 32 próbek:

```text
35 B  status i nagłówek measurement
288 B raw XYZ24
T B   timing evidence, jeszcze niezamrożone
14 B  raw framing header + CRC
COBS + delimiter
```

Dla timing evidence 40 B cała odpowiedź ma około 380 B wire, a request i
response około 422 B, czyli około 36.6 ms. Koszt jest porównywalny ze starą
ramką danych, ale znika grant response, osobny commit i seria ramek niepowiązana
z requestem.

Przy większym backlogu limit 512 B pozwala orientacyjnie przenieść do 48 próbek
z takim nagłówkiem czasu. Dokładne `max_samples` wybierze scheduler po pomiarach;
nie kodujemy 32 ani 48 na stałe w kontrakcie.

## 9. Granice odpowiedzialności

Measurement Service nie odpowiada za:

- framing, COBS i CRC;
- wybór następnego node na channel;
- wyznaczanie modeli czasu;
- format pliku Capture;
- konfigurację sensora;
- update firmware.

Udostępnia zweryfikowany ciągły chunk i stan retencji. `NodeSession` odpowiada za
trwały zapis, przesuwanie cursorów i retry, a scheduler za moment i rozmiar pull.
