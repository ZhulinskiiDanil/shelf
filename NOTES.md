# Notes — спорные решения

- **Single instance.** Mutex `Local\Shelf.SingleInstance`. Новая копия шлёт старой `WM_APP_QUIT` и ждёт mutex до 5 с. Так запуск свежей `shelf-mN.exe` сразу заменяет старую.
- **Главное окно** — скрытое top-level окно, а не message-only, иначе не приходит широковещательное `TaskbarCreated` (иконка пропала бы после перезапуска Explorer).
- **PrtScrn через `RegisterHotKey`** часто занят (Snipping Tool в Windows 11, Lightshot): `ERROR_HOTKEY_ALREADY_REGISTERED`. В M0 это только предупреждение в логе; с M1 захват идёт через low-level keyboard hook, который подавляет системный обработчик.
- **Hot-reload конфига** — `FindFirstChangeNotificationW` на папке `%APPDATA%\Shelf` в отдельном потоке (без поллинга, CPU в простое ≈ 0), с паузой 150 мс на сохранение редактором. Ошибка парсинга — дефолты + запись в лог.
- **Exe в git не попадают** (`dist/` в `.gitignore`); сборки вех прикладываются к GitHub-релизам `mN`.
