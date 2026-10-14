# Changelog

## M0 — Каркас (`dist/shelf-m0.exe`)

- Трей-иконка с меню: «Настройки (config.toml)», «Открыть папку истории», «Выход».
- Single instance через именованный mutex: новая сборка просит старую завершиться и занимает её место (удобно менять `shelf-mN.exe` без ручного выхода).
- Конфиг `%APPDATA%\Shelf\config.toml`: создаётся с дефолтами, перечитывается при сохранении.
- Автозапуск (`autostart = true`, ключ `HKCU\...\Run`), по умолчанию выключен.
- Глобальные хоткеи из конфига (`[hotkeys]`), лог в `%APPDATA%\Shelf\logs\shelf.log`, уровень `log_level`.

### Как проверить
1. Запустите `shelf-m0.exe` — в трее появится иконка, ПКМ открывает меню.
2. «Настройки» открывает `config.toml`; поставьте `log_level = "debug"`, сохраните — в логе появится `config reloaded`.
3. Нажмите `Ctrl+Shift+Space` — в логе `shelf hotkey`.
4. Запустите `shelf-m0.exe` второй раз — первая копия завершится (в логе `asked to quit by a newer instance`), иконка в трее одна.
5. `autostart = true` → в `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` появится `Shelf`.
