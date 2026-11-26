# IA en lenguaje natural: endpoint configurable (todas las plataformas)

Contrato único. Implementación de referencia: [`linux/src/ai.rs`](../linux/src/ai.rs) (la usa Android). Windows ya tiene búsqueda con Groq (`src/ai.rs`, clave cifrada con DPAPI) pero **con la URL fija**; macOS no tiene IA todavía. Ambos deben migrar a este contrato (ver §6).

## 1. Qué se configura

Lista ordenada de *endpoints*; se prueba el primero habilitado y, si falla por completo, el siguiente.

| Campo | Significado |
|---|---|
| `name` | Etiqueta libre |
| `base_url` | Raíz de la API compatible con OpenAI, sin `/chat/completions` (`https://api.groq.com/openai/v1`) |
| `api_key` | Vacía en IA local. Se muestra enmascarada (`gsk_…1234`); reenviar el valor enmascarado conserva la clave guardada |
| `mode` | `auto` · `list` · `force` (abajo) |
| `model` | Modelo elegido (`list`) o forzado (`force`); opcional en `auto` |
| `fallback_models` | Cadena explícita de respaldo |
| `fallback` | Si un modelo falla, probar otro (por defecto sí) |
| `tools` | El modelo soporta llamadas a herramientas. `false` = modo traducción |
| `local` | Permite `http://` y no exige clave |
| `timeout_s` | 90 por defecto |

Presets que solo rellenan `base_url`: `groq`, `ollama` (`:11434/v1`), `lmstudio` (`:1234/v1`), `llamacpp` (`:8080/v1`), `vllm` (`:8000/v1`), `custom`. Sirve cualquier servicio compatible: OpenRouter (modelos `:free`), Cerebras, Gemini en modo OpenAI, OpenAI, etc.

## 2. Elección de modelo

- **auto:** `GET {base_url}/models`, descarta no-chat (whisper, tts, embeddings, guard…), ordena por una lista de preferencia y luego por parámetros declarados en el nombre (`70b` > `8b`).
- **list:** el usuario elige de esa lista (pantalla *Ver modelos*); ese va primero y el resto queda de respaldo.
- **force:** se usa el nombre tal cual, **sin consultar `/models`** (APIs que no lo ofrecen). Respaldo solo con `fallback_models`.
- Si `/models` falla pero hay un modelo explícito, se usa igual.

## 3. Fallback

- 429, 5xx, timeout, respuesta vacía o demasiados pasos → siguiente modelo. Un modelo que dio 429 pasa al final de la cola durante 60 s.
- 401/403 → no se prueban más modelos de ese endpoint (la clave no mejora cambiando de modelo); se pasa al siguiente endpoint.
- `fallback=false` limita a un único modelo.

## 4. Herramientas (solo lectura)

`search`, `count`, `largest`, `recent`, `stats` sobre el índice, con la misma sintaxis de consulta de cada plataforma (`path:`, `ext:`, `size:>`). El modelo solo ve **nombres y rutas**, nunca contenido. Hasta 8 pasos; salida de cada herramienta limitada a 4 000 caracteres.

**Modo traducción (`tools=false`):** para modelos locales chicos sin funciones. El modelo devuelve solo `{"query","sort","limit"}`; CensusIndex ejecuta la consulta y arma la respuesta.

## 5. Seguridad

- `http://` solo con `local=true`: con una clave, `http://` la expondría.
- La clave se guarda en un archivo privado de la app (`0600` en Unix, directorio privado en Android). **No** está cifrada con Android Keystore: pendiente. En Windows sigue DPAPI.
- Se envían al proveedor la pregunta y los resultados de las herramientas (rutas y nombres). Con IA local no sale nada del equipo.

## 6. Estado por plataforma

| Plataforma | Estado |
|---|---|
| Linux / Android | Implementado y probado (17 pruebas contra un servidor falso: auto, list, force, fallback, 401→siguiente endpoint, sin cabecera de autorización en local, traducción, validación, clave enmascarada) |
| Windows | Pendiente de migrar `src/ai.rs` (hoy `API` es constante). No se tocó: no se puede compilar aquí |
| macOS | Pendiente (el crate `mac/` no tiene IA) |
