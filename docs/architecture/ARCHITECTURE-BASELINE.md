# Sensor System architecture baseline

- Baseline: `architecture-v1`
- Status: frozen for implementation
- Date: 2026-09-28

Baseline zamraża granice odpowiedzialności i wymagania produktu. Nie zamraża
szczegółów implementacyjnych, które powinny powstać razem z właściwym modułem.

## Zamrożone decyzje

- wspólny język mode/state/health/command/result/event/metric/snapshot;
- kompozycja zamiast wspólnej hierarchii obiektów systemowych;
- jeden właściciel każdego zmiennego stanu;
- oddzielenie `sensor_core`, `sensor_protocol`, `sensor_data`, `system_config`,
  adapterów, aplikacji i narzędzi;
- równoległa implementacja `host_v2` bez importów starego `host`;
- firmware node w C++17/Pico SDK, krytyczny runtime hosta w Rust, Python tylko
  dla narzędzi i przetwarzania offline;
- początkowo jeden proces recordera z logicznym `ChannelAgent` per channel i
  możliwością późniejszej izolacji procesu per `TransportDevice`;
- application protocol 5 i framing protocol 3 dla nowej generacji;
- topologia Host -> TransportDevice -> Channel -> Node -> Sensor;
- jeden `ChannelAgent` posiadający jeden transport binding;
- wiele `NodeSession` i jeden scheduler w channel;
- niezależny profil obserwowalności Production/Diagnostic;
- lifecycle obejmujący Idle, Recording, przejściowy Stopping, Maintenance i
  Faulted;
- jeden bounded `MeasurementJournal` jako retencja niepotwierdzonych próbek i
  dokładnych luk node;
- trwały commit dopiero po trwałym zapisie hosta;
- surowe XYZ, sekwencja, jawne luki i źródłowe dowody czasu;
- lokalny Operations API jako jedyna droga sterowania;
- możliwość zatrzymania pomiarów podczas Maintenance;
- Capture, Archive i Derived jako różne produkty danych.

Dokumenty należące do baseline:

- [PRODUCT-INVARIANTS.md](PRODUCT-INVARIANTS.md);
- [DOMAIN-LANGUAGE.md](DOMAIN-LANGUAGE.md);
- [HOST-V2-STRUCTURE.md](HOST-V2-STRUCTURE.md);
- [MEASUREMENT-DATA-CONTRACT.md](MEASUREMENT-DATA-CONTRACT.md);
- [RS485-COMMUNICATION-POLICY.md](RS485-COMMUNICATION-POLICY.md);
- [HOST-SCALING.md](HOST-SCALING.md).

## Świadomie niezamrożone szczegóły

- dokładny układ bajtów framingu i application messages;
- liczba próbek w transfer chunk;
- fizyczny format Capture, chunk size i algorytm kompresji;
- fizyczne powtarzanie `sample_seq` przy każdym wierszu;
- częstotliwość timing anchors i algorytm dopasowania zegara;
- rozmiar `MeasurementJournal` oraz wynikający czas retencji;
- dokładny algorytm wag schedulera;
- technologia lokalnego IPC;
- dokładne kryterium wydzielenia `TransportDevice` do osobnego procesu;
- szczegóły przyszłych adapterów innych niż pierwszy RS485;
- UI, wygląd statusu i produkty analityczne.

Te decyzje powstają z testem w momencie budowy odpowiedniego modułu. Nie mogą
odwrócić kierunku zależności ani utworzyć drugiego właściciela stanu.

## Zmiana baseline

Zmiana nazwy lub doprecyzowanie bez zmiany zachowania wymaga aktualizacji
dokumentacji. Zmiana odpowiedzialności, niezmiennika trwałości, semantyki danych
albo kierunku zależności wymaga ADR opisującego przyczynę i konsekwencje.

„Frozen” nie oznacza zakazu rozwoju. Oznacza, że nowe funkcje trafiają do
istniejących granic, a zmianę fundamentu wykonujemy świadomie zamiast przez
lokalny wyjątek w kodzie.
