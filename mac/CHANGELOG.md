# Changelog — CensusIndex macOS

## 0.1.0 — alpha (2026-11-22)

- Crate `mac/` independiente, contrato de `docs/PLATAFORMAS.md`.
- `FileKey`/`FileId`, `Entrada` por lotes, nombres en bytes + `encoding`.
- `Index`/`Builder` con `Compacto` de 12 B, hijos CSR, orden por nombre,
  subárboles Euler y tamaños recursivos; hardlinks contados una vez.
- Consulta `path:`, `file:`, `folder:`, `ext:`, `size:`, `dm:`, `rc:`, términos,
  orden y paginación; plegado idéntico a Windows.
- Fuente macOS `getattrlistbulk` con workers, sin `lstat` por archivo.
- Volúmenes: `getmntinfo`, `MNT_LOCAL`, `ATTR_VOL_UUID`, nube, red.
- §10: `DiskHwId` desde `diskutil info -plist`, `volume_id` por
  disco/offset/UUID, `sin_identidad` fuera de federación.
- §9: `cloud_id` compartido, parser de `CloudStorage`/`Shared drives`, alias
  para rclone, duplicados avisados.
- CLI mínima: `version`, `volumes`, `cloud-id`, `fold`.
- CI para Intel y Apple silicon; scripts de universal2, `.app`, firma y
  notarización.

Pendiente: `.cnx`, catálogo por volumen, `export --wcat`, CLI completa,
FSEvents, HTTP/MCP, Tailscale/Atlas y pruebas en VM. Ver `PENDIENTE.md`.
