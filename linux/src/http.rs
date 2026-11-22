//! Servidor HTTP para Tailscale (`/health`, `/api/v1/*`, `/mcp`, snapshot) y
//! cliente para consultar peers.
//!
//! PENDIENTE. Se expone en loopback y se publica con `tailscale serve --bg`
//! para tener HTTPS, MagicDNS e identidad sin abrir puertos a la LAN.
