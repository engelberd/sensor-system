# Sensor System — instrukcja operatora

Ta instrukcja opisuje codzienną obsługę gotowej instalacji. Instalację systemu,
konfigurację portów RS485 i aktualizację firmware'u powinien wykonać opiekun
techniczny.

## Uruchomienie

1. Włącz komputer hosta i zasilanie czujników.
2. Otwórz panel pod adresem przekazanym przez opiekuna instalacji, zwykle
   `http://<adres-hosta>:8090/`.
3. Poczekaj, aż aktywne kanały pokażą stan uruchomiony, a czujniki stan online i
   aktywny przepływ próbek.
4. Sprawdź, czy panel nie pokazuje alarmów oraz czy rośnie liczba zapisanych
   próbek.

## Znaczenie podstawowych stanów

- `ONLINE` — host komunikuje się z czujnikiem.
- `ONLINE / BRAK PRÓBEK` — czujnik odpowiada, ale dane pomiarowe nie napływają;
  zapis nie jest poprawny i wymaga reakcji.
- `OFFLINE` — brak komunikacji z czujnikiem.
- `NO-RUNTIME` lub `brak runtime` — nie ma aktualnego statusu procesu; sprawdź
  supervisor lub skontaktuj się z opiekunem.

## Bezpieczna reakcja na problem

1. Zanotuj kanał, numer czujnika, godzinę i treść alarmu.
2. Nie odłączaj nośnika danych i nie usuwaj plików runtime ani logów.
3. Przy stanie `ONLINE / BRAK PRÓBEK` użyj `Restart firmware` tylko raz i poczekaj
   na powrót zapisu. Operacja chwilowo zatrzymuje recorder tego kanału.
4. Jeśli alarm wraca, czujnik pozostaje offline albo recorder nie wznowił pracy,
   nie powtarzaj restartów — przekaż zdarzenie opiekunowi technicznemu.

## Zakończenie pracy

Do zwykłej pracy ciągłej nie wyłączaj supervisora. Jeżeli instalacja ma zostać
wyłączona, najpierw zatrzymaj rejestrowanie w panelu, zaczekaj na potwierdzenie,
a dopiero potem wyłącz komputer i zasilanie urządzeń.

## Zasady bezpieczeństwa

- Panel jest przeznaczony wyłącznie dla zaufanej sieci zakładowej i nie może być
  wystawiony bezpośrednio do Internetu.
- Nie zmieniaj identyfikatorów węzłów, ODR, zakresu ani portów bez uzgodnienia z
  opiekunem instalacji.
- Aktualizacja firmware'u i kasowanie danych nie należą do rutynowych czynności
  operatora.
