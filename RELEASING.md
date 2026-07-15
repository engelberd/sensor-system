# Przygotowanie wydania

1. Zaktualizuj `VERSION`, `host/common/version.py`, wersję firmware'u w
   `node/common/protocol_ids.h` oraz `CHANGELOG.md`.
2. Uruchom `./tools/release_check.sh`.
3. Sprawdź ręcznie dashboard, panel operatora, zapis danych oraz restart jednego
   węzła na stanowisku testowym.
4. Zatwierdź tylko pliki produktu. Nie dodawaj `host/system_config.json`,
   lokalnych unitów systemd, danych pomiarowych ani katalogów build.
5. Utwórz podpisany lub opisany tag `vX.Y.Z` na zatwierdzonym commicie.
6. Wypchnij commit i tag, a następnie utwórz GitHub Release z opisem z
   `CHANGELOG.md` oraz plikami:

   - `node/build/releases/sensor-system-node-vX.Y.Z.zip`
   - `node/build/releases/sensor-system-node-vX.Y.Z.zip.sha256`

Wydanie sprzętowe wymaga dodatkowo wykonania checklisty pre-flight z
`host/README-deploy.md`; testy automatyczne nie zastępują testu RS485, restartu
zasilania ani próby zapisu na docelowym nośniku.
