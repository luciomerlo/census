//! Overrides manuales de Atlas (`docs/ATLAS.md` §9.5).
//!
//! La detección automática se equivoca: un Google Drive compartido montado por
//! dos programas, un puente USB que repite el número de serie, una PC que no
//! responde. Cuando eso pasa, el usuario tiene la última palabra y **su decisión
//! manda sobre la detección**: en Atlas, un duplicado visible es peor que un
//! volumen faltante avisado.
//!
//! Todo se guarda en `%LOCALAPPDATA%\Census\atlas\`:
//! - `overrides.json`: excluir / incluir / unir / separar, con motivo y fecha.
//! - `seleccion.json`: qué volúmenes remotos offline el usuario tildó para leer.
//!
//! Las reglas se aplican **por `(pc, volumen)`**, nunca por letra de unidad
//! (§10): la letra es presentación y cambia.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Qué hacer con un volumen de una PC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accion {
    /// No leerlo ni mostrarlo (típico: es el mismo drive que ya leo de otra PC).
    Excluir,
    /// Deshacer una exclusión anterior.
    Incluir,
    /// Tratar este volumen como si fuera otro (mismo contenido, dos montajes).
    Unir,
    /// Deshacer una unión: vuelve a ser un volumen propio.
    Separar,
}

impl Accion {
    pub fn como_str(self) -> &'static str {
        match self {
            Accion::Excluir => "excluir",
            Accion::Incluir => "incluir",
            Accion::Unir => "unir",
            Accion::Separar => "separar",
        }
    }

    pub fn desde_str(s: &str) -> Option<Accion> {
        Some(match s {
            "excluir" => Accion::Excluir,
            "incluir" => Accion::Incluir,
            "unir" => Accion::Unir,
            "separar" => Accion::Separar,
            _ => return None,
        })
    }
}

/// Una decisión del usuario sobre un volumen remoto.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Regla {
    pub pc: String,
    /// `volume_id` o `cloud_id`; nunca una letra.
    pub volumen: String,
    pub accion: Accion,
    /// Sólo para `Unir`: el volumen canónico al que se pliega.
    pub otro: Option<String>,
    pub motivo: Option<String>,
    /// Segundos desde la época. Sirve para saber cuándo se decidió, no para
    /// ordenar versiones (§4).
    pub fecha: i64,
}

/// Conjunto de reglas. Gana siempre la última regla de cada `(pc, volumen)`.
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    reglas: Vec<Regla>,
}

impl Overrides {
    pub fn nuevas() -> Self {
        Overrides::default()
    }

    /// Agrega una regla, reemplazando la anterior del mismo `(pc, volumen)`.
    /// "Gana la última decisión" es más fácil de explicar que acumular reglas.
    pub fn decidir(&mut self, regla: Regla) {
        self.reglas
            .retain(|r| !(r.pc == regla.pc && r.volumen == regla.volumen));
        self.reglas.push(regla);
    }

    pub fn reglas(&self) -> &[Regla] {
        &self.reglas
    }

    fn ultima(&self, pc: &str, volumen: &str) -> Option<&Regla> {
        self.reglas
            .iter()
            .rev()
            .find(|r| r.pc == pc && r.volumen == volumen)
    }

    /// El volumen está excluido a mano. Manda sobre cualquier detección.
    pub fn excluido(&self, pc: &str, volumen: &str) -> bool {
        matches!(
            self.ultima(pc, volumen).map(|r| r.accion),
            Some(Accion::Excluir)
        )
    }

    /// Si el volumen fue unido a otro, a cuál. Resuelve cadenas y corta ciclos.
    pub fn canonico(&self, pc: &str, volumen: &str) -> String {
        let mut actual = volumen.to_string();
        let mut vistos = vec![actual.clone()];
        loop {
            let siguiente = self
                .reglas
                .iter()
                .rev()
                .find(|r| r.pc == pc && r.volumen == actual && r.accion == Accion::Unir)
                .and_then(|r| r.otro.clone());
            match siguiente {
                Some(s) if !vistos.contains(&s) => {
                    vistos.push(s.clone());
                    actual = s;
                }
                // Ciclo o sin regla: se devuelve el último alcanzado.
                _ => return actual,
            }
        }
    }

    /// Lista de volúmenes excluidos, para diagnóstico y para la UI.
    pub fn excluidos(&self) -> Vec<(&str, &str, Option<&str>)> {
        self.reglas
            .iter()
            .filter(|r| r.accion == Accion::Excluir)
            .map(|r| (r.pc.as_str(), r.volumen.as_str(), r.motivo.as_deref()))
            .collect()
    }

    // ------------------------------------------------------------- JSON

    pub fn a_json(&self) -> Value {
        let reglas: Vec<Value> = self
            .reglas
            .iter()
            .map(|r| {
                json!({
                    "pc": r.pc,
                    "volumen": r.volumen,
                    "accion": r.accion.como_str(),
                    "otro": r.otro,
                    "motivo": r.motivo,
                    "fecha": r.fecha,
                })
            })
            .collect();
        json!({ "version": 1, "reglas": reglas })
    }

    pub fn de_json(v: &Value) -> Result<Self, String> {
        let lista = v
            .get("reglas")
            .and_then(Value::as_array)
            .ok_or_else(|| "overrides: falta `reglas`".to_string())?;
        let mut o = Overrides::nuevas();
        for (i, r) in lista.iter().enumerate() {
            let pc = r
                .get("pc")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("overrides: regla {i} sin `pc`"))?;
            let volumen = r
                .get("volumen")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("overrides: regla {i} sin `volumen`"))?;
            let accion_s = r
                .get("accion")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("overrides: regla {i} sin `accion`"))?;
            let accion = Accion::desde_str(accion_s)
                .ok_or_else(|| format!("overrides: acción desconocida `{accion_s}`"))?;
            o.decidir(Regla {
                pc: pc.to_string(),
                volumen: volumen.to_string(),
                accion,
                otro: r.get("otro").and_then(Value::as_str).map(str::to_string),
                motivo: r.get("motivo").and_then(Value::as_str).map(str::to_string),
                fecha: r.get("fecha").and_then(Value::as_i64).unwrap_or(0),
            });
        }
        Ok(o)
    }

    pub fn cargar(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(t) => {
                let v: Value = serde_json::from_str(&t)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                Overrides::de_json(&v)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            }
            // Sin archivo es lo normal la primera vez: no es un error.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Overrides::nuevas()),
            Err(e) => Err(e),
        }
    }

    /// Guarda con `tmp` + rename: si se corta la luz a mitad, el archivo viejo
    /// queda intacto en vez de quedar truncado.
    pub fn guardar(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            let texto = serde_json::to_string_pretty(&self.a_json())
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            f.write_all(texto.as_bytes())?;
            f.write_all(b"\n")?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    }
}

// ------------------------------------------------------- selección offline

/// Qué volúmenes offline de otras PCs el usuario tildó para leer (§9.4).
///
/// Por defecto **nada** está tildado: los offline no se consultan ni se
/// muestran si el usuario no lo pidió explícitamente.
#[derive(Clone, Debug, Default)]
pub struct Seleccion {
    tildados: Vec<(String, String)>,
}

impl Seleccion {
    pub fn nuevas() -> Self {
        Seleccion::default()
    }

    pub fn tildar(&mut self, pc: &str, volumen: &str) {
        let clave = (pc.to_string(), volumen.to_string());
        if !self.tildados.contains(&clave) {
            self.tildados.push(clave);
        }
    }

    pub fn destildar(&mut self, pc: &str, volumen: &str) {
        self.tildados
            .retain(|(p, v)| !(p == pc && v == volumen));
    }

    pub fn esta(&self, pc: &str, volumen: &str) -> bool {
        self.tildados.iter().any(|(p, v)| p == pc && v == volumen)
    }

    pub fn listar(&self) -> &[(String, String)] {
        &self.tildados
    }

    pub fn a_json(&self) -> Value {
        let items: Vec<Value> = self
            .tildados
            .iter()
            .map(|(pc, v)| json!({ "pc": pc, "volumen": v }))
            .collect();
        json!({ "version": 1, "tildados": items })
    }

    pub fn de_json(v: &Value) -> Result<Self, String> {
        let lista = v
            .get("tildados")
            .and_then(Value::as_array)
            .ok_or_else(|| "seleccion: falta `tildados`".to_string())?;
        let mut s = Seleccion::nuevas();
        for r in lista {
            let pc = r.get("pc").and_then(Value::as_str).unwrap_or("");
            let vol = r.get("volumen").and_then(Value::as_str).unwrap_or("");
            if !pc.is_empty() && !vol.is_empty() {
                s.tildar(pc, vol);
            }
        }
        Ok(s)
    }

    pub fn cargar(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(t) => {
                let v: Value = serde_json::from_str(&t)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                Seleccion::de_json(&v)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Seleccion::nuevas()),
            Err(e) => Err(e),
        }
    }

    pub fn guardar(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            let texto = serde_json::to_string_pretty(&self.a_json())
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            f.write_all(texto.as_bytes())?;
            f.write_all(b"\n")?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    }
}

// ------------------------------------------------------------------ rutas

/// Directorio de Atlas: `%LOCALAPPDATA%\Census\atlas`.
pub fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    Some(base.join("Census").join("atlas"))
}

pub fn ruta_overrides() -> Option<PathBuf> {
    dir().map(|d| d.join("overrides.json"))
}

pub fn ruta_seleccion() -> Option<PathBuf> {
    dir().map(|d| d.join("seleccion.json"))
}

/// Agrupa volúmenes por canónico, aplicando exclusiones. Es la vista que
/// consumen la consulta federada y la UI: una entrada por volumen real.
pub fn agrupar(
    ov: &Overrides,
    items: &[(String, String)],
) -> Vec<(String, Vec<(String, String)>)> {
    let mut grupos: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (pc, vol) in items {
        if ov.excluido(pc, vol) {
            continue;
        }
        let clave = format!("{pc}\u{1f}{}", ov.canonico(pc, vol));
        grupos.entry(clave).or_default().push((pc.clone(), vol.clone()));
    }
    let mut salida: Vec<(String, Vec<(String, String)>)> = grupos
        .into_iter()
        .map(|(k, v)| {
            let canon = k.split('\u{1f}').nth(1).unwrap_or("").to_string();
            (canon, v)
        })
        .collect();
    salida.sort_by(|a, b| a.0.cmp(&b.0));
    salida
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regla(pc: &str, vol: &str, accion: Accion) -> Regla {
        Regla {
            pc: pc.into(),
            volumen: vol.into(),
            accion,
            otro: None,
            motivo: Some("prueba".into()),
            fecha: 1_700_000_000,
        }
    }

    #[test]
    fn exclusión_manda() {
        let mut o = Overrides::nuevas();
        assert!(!o.excluido("PC-B", "gdrive:abc"));
        o.decidir(regla("PC-B", "gdrive:abc", Accion::Excluir));
        assert!(o.excluido("PC-B", "gdrive:abc"));
        // Otra PC con el mismo cloud_id no queda afectada.
        assert!(!o.excluido("PC-C", "gdrive:abc"));
    }

    #[test]
    fn gana_la_ultima_decision() {
        let mut o = Overrides::nuevas();
        o.decidir(regla("PC-B", "v1", Accion::Excluir));
        o.decidir(regla("PC-B", "v1", Accion::Incluir));
        assert!(!o.excluido("PC-B", "v1"));
        o.decidir(regla("PC-B", "v1", Accion::Excluir));
        assert!(o.excluido("PC-B", "v1"));
        // No se acumulan reglas del mismo volumen.
        assert_eq!(o.reglas().len(), 1);
    }

    #[test]
    fn union_en_cadena() {
        let mut o = Overrides::nuevas();
        let mut r = regla("PC-B", "v2", Accion::Unir);
        r.otro = Some("v1".into());
        o.decidir(r);
        let mut r = regla("PC-B", "v3", Accion::Unir);
        r.otro = Some("v2".into());
        o.decidir(r);
        assert_eq!(o.canonico("PC-B", "v2"), "v1");
        assert_eq!(o.canonico("PC-B", "v3"), "v1");
        // Separar corta la cadena en ese volumen.
        o.decidir(regla("PC-B", "v2", Accion::Separar));
        assert_eq!(o.canonico("PC-B", "v2"), "v2");
        // v3 sigue apuntando a v2, que ahora es propio: la cadena se corta ahí.
        assert_eq!(o.canonico("PC-B", "v3"), "v2");
    }

    #[test]
    fn ciclo_no_cuelga() {
        let mut o = Overrides::nuevas();
        let mut r = regla("P", "a", Accion::Unir);
        r.otro = Some("b".into());
        o.decidir(r);
        let mut r = regla("P", "b", Accion::Unir);
        r.otro = Some("a".into());
        o.decidir(r);
        // Termina en algún punto de la cadena, sin loop infinito.
        let c = o.canonico("P", "a");
        assert!(c == "a" || c == "b");
    }

    #[test]
    fn json_ida_y_vuelta() {
        let dir = std::env::temp_dir().join(format!("census-atlas-{}", std::process::id()));
        let path = dir.join("overrides.json");
        let mut o = Overrides::nuevas();
        let mut r = regla("PC-B", "cloud:xyz", Accion::Unir);
        r.otro = Some("cloud:canon".into());
        o.decidir(r);
        o.decidir(regla("PC-C", "vol:3", Accion::Excluir));
        o.guardar(&path).expect("guardar");
        let leido = Overrides::cargar(&path).expect("cargar");
        assert_eq!(leido.reglas(), o.reglas());
        assert!(leido.excluido("PC-C", "vol:3"));
        assert_eq!(leido.canonico("PC-B", "cloud:xyz"), "cloud:canon");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sin_archivo_es_vacio() {
        let path = std::env::temp_dir().join("census-atlas-no-existe/overrides.json");
        let o = Overrides::cargar(&path).expect("sin archivo no es error");
        assert!(o.reglas().is_empty());
    }

    #[test]
    fn agrupar_colapsa_y_excluye() {
        let mut o = Overrides::nuevas();
        let mut r = regla("PC-B", "d2", Accion::Unir);
        r.otro = Some("d1".into());
        o.decidir(r);
        o.decidir(regla("PC-C", "d9", Accion::Excluir));
        let items = vec![
            ("PC-B".to_string(), "d1".to_string()),
            ("PC-B".to_string(), "d2".to_string()),
            ("PC-C".to_string(), "d9".to_string()),
        ];
        let grupos = agrupar(&o, &items);
        assert_eq!(grupos.len(), 1);
        assert_eq!(grupos[0].0, "d1");
        assert_eq!(grupos[0].1.len(), 2);
    }

    #[test]
    fn seleccion_offline_empieza_vacia() {
        let mut s = Seleccion::nuevas();
        assert!(!s.esta("PC-B", "d1"));
        s.tildar("PC-B", "d1");
        s.tildar("PC-B", "d1");
        assert_eq!(s.listar().len(), 1);
        assert!(s.esta("PC-B", "d1"));
        s.destildar("PC-B", "d1");
        assert!(!s.esta("PC-B", "d1"));
    }
}
