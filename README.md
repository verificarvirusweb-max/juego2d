# Breakout2D MVP

## Obtener el .exe
**Opcion A (sin instalar nada): GitHub Actions**
1. Crea un repo en GitHub y sube esta carpeta completa.
2. Pestana Actions > build-windows > Run workflow.
3. Al terminar, descarga el artifact `breakout2d-windows` (trae breakout2d.exe, host.bat, join.bat).

**Opcion B (en tu laptop con Windows):**
1. Instala Rust desde https://rustup.rs (con las Build Tools de Visual Studio C++).
2. En esta carpeta: `cargo build --release -p breakout2d`
3. El exe queda en `target\release\breakout2d.exe`.

## Jugar (hasta 4 humanos + 3 bots)
- Host: doble clic en `host.bat` (acepta el aviso del firewall de Windows).
- Otros: `join.bat` e ingresan la IP local del host (`ipconfig`).
- Controles: WASD mover, click disparar, R recargar, E saquear cadaveres cercanos, Q cambiar de arma
  (con una de la mochila), 1/2 tipo de municion para recargar, F5 (host) reinicia la raid.
- Extraccion: quedate 3 s en la zona verde. Puedes probar solo con el host y los bots.

## Red
Todos en la misma red local o Wi-Fi. Starlink usa CGNAT: no se puede alojar por internet sin
una VPN (Tailscale o ZeroTier). Con Tailscale usa la IP 100.x del host.

## Lo que incluye / no incluye el MVP
Incluye: armaduras con durabilidad, 2 municiones con penetracion, zonas de impacto, bots,
extraccion, host autoritativo, 2 armas con cargador y recarga, municion limitada, mochila de 8
slots y saqueo de cadaveres (armas, municion, valiosos). No incluye: mercado, curacion, granadas,
mejoras de armas, penetracion de paredes, prediccion de cliente. Eso sigue la hoja de ruta de docs/DESIGN.md.
