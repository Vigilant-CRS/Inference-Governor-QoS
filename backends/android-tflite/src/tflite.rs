//! Die TFLite-C-API, zur Laufzeit geladen.
//!
//! `libtensorflowlite_jni.so` exportiert die C-API (`TfLiteModel*`,
//! `TfLiteInterpreter*`, `TfLiteTensor*`), `libtensorflowlite_gpu_jni.so` den
//! GPU-Delegate V2. Beide stammen unveraendert aus den AARs von Maven Central
//! (tools/android/fetch-assets.sh) und liegen neben dem Binary, nie im
//! Repository.
//!
//! Hier und nur hier steht `unsafe` (ADR-0033): der Governor spricht mit
//! diesem Prozess ueber OIP und weiss nichts von Zeigern.

#![allow(unsafe_code)]

use libloading::os::unix::{Library, RTLD_GLOBAL, RTLD_NOW};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

/// Ein undurchsichtiger TFLite-Typ. Nur als Zeiger verwendet.
#[repr(C)]
pub(crate) struct Opaque {
    _private: [u8; 0],
}

type Status = c_int;
const OK: Status = 0;

/// Die Rechenart eines Interpreters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Accelerator {
    /// GPU-Delegate V2. Lehnt der Delegate das Modell ab, ist das ein Fehler
    /// und kein stiller Rueckfall auf die CPU.
    Gpu {
        /// FP32 statt FP16 auf der GPU.
        full_precision: bool,
    },
    /// Nur CPU, ausdruecklich zum Vergleich.
    Cpu,
}

impl Accelerator {
    /// Die Plattformbezeichnung in den Modellmetadaten.
    pub(crate) const fn platform(self) -> &'static str {
        match self {
            Self::Gpu { .. } => "tflite_gpu",
            Self::Cpu => "tflite_cpu",
        }
    }
}

/// `TfLiteGpuDelegateOptionsV2` aus `delegates/gpu/delegate_options.h`.
///
/// Die Felder bis `model_token` sind seit TFLite 2.4 stabil. Spaetere
/// Versionen haengen Felder an; `tail` haelt dafuer Platz frei. Das ist
/// ausreichend, weil die Struktur nur als Zeiger in die Bibliothek geht und
/// `TfLiteGpuDelegateOptionsV2Default` sie ueber einen vom Aufrufer
/// bereitgestellten Speicher zurueckgibt (AAPCS64: Rueckgabe groesser 16 Byte
/// ueber `x8`) — sie schreibt ihre wirkliche Groesse, nicht unsere.
#[repr(C)]
#[allow(dead_code, reason = "Felder werden nur von der Bibliothek gelesen")]
struct GpuOptions {
    is_precision_loss_allowed: i32,
    inference_preference: i32,
    inference_priority1: i32,
    inference_priority2: i32,
    inference_priority3: i32,
    experimental_flags: i64,
    max_delegated_partitions: i32,
    serialization_dir: *const c_char,
    model_token: *const c_char,
    tail: [u64; 32],
}

const PREFERENCE_SUSTAINED_SPEED: i32 = 1;
const PRIORITY_AUTO: i32 = 0;
const PRIORITY_MAX_PRECISION: i32 = 1;
const PRIORITY_MIN_LATENCY: i32 = 2;
const FLAG_ENABLE_QUANT: i64 = 1;
/// Das Pixel 2 hat kein oeffentliches OpenCL; der Delegate soll es gar nicht
/// erst suchen.
const FLAG_GL_ONLY: i64 = 1 << 2;

/// Die geladenen Einsprungpunkte.
///
/// Wird einmal geladen und nie entladen (`Box::leak`): jeder Interpreter
/// haelt Zeiger in diese Bibliotheken.
pub(crate) struct Api {
    model_create_from_file: unsafe extern "C" fn(*const c_char) -> *mut Opaque,
    model_delete: unsafe extern "C" fn(*mut Opaque),
    options_create: unsafe extern "C" fn() -> *mut Opaque,
    options_delete: unsafe extern "C" fn(*mut Opaque),
    options_set_num_threads: unsafe extern "C" fn(*mut Opaque, i32),
    options_add_delegate: unsafe extern "C" fn(*mut Opaque, *mut Opaque),
    interpreter_create: unsafe extern "C" fn(*const Opaque, *const Opaque) -> *mut Opaque,
    interpreter_delete: unsafe extern "C" fn(*mut Opaque),
    allocate_tensors: unsafe extern "C" fn(*mut Opaque) -> Status,
    invoke: unsafe extern "C" fn(*mut Opaque) -> Status,
    input_count: unsafe extern "C" fn(*const Opaque) -> i32,
    input_tensor: unsafe extern "C" fn(*const Opaque, i32) -> *mut Opaque,
    output_count: unsafe extern "C" fn(*const Opaque) -> i32,
    output_tensor: unsafe extern "C" fn(*const Opaque, i32) -> *const Opaque,
    tensor_type: unsafe extern "C" fn(*const Opaque) -> c_int,
    tensor_num_dims: unsafe extern "C" fn(*const Opaque) -> i32,
    tensor_dim: unsafe extern "C" fn(*const Opaque, i32) -> i32,
    tensor_byte_size: unsafe extern "C" fn(*const Opaque) -> usize,
    tensor_name: unsafe extern "C" fn(*const Opaque) -> *const c_char,
    copy_from_buffer: unsafe extern "C" fn(*mut Opaque, *const c_void, usize) -> Status,
    copy_to_buffer: unsafe extern "C" fn(*const Opaque, *mut c_void, usize) -> Status,
    gpu: Option<GpuApi>,
    _core: Library,
}

struct GpuApi {
    options_default: unsafe extern "C" fn() -> GpuOptions,
    create: unsafe extern "C" fn(*const GpuOptions) -> *mut Opaque,
    delete: unsafe extern "C" fn(*mut Opaque),
    _library: Library,
}

/// Holt einen Einsprungpunkt als Funktionszeiger.
///
/// # Safety
///
/// `T` muss die C-Signatur des Symbols exakt wiedergeben, und die Bibliothek
/// muss laenger leben als jeder Aufruf des Zeigers.
unsafe fn symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T, String> {
    // SAFETY: vom Aufrufer zugesichert (Signatur); die Bibliothek wird in
    // `Api` gehalten und nie entladen.
    unsafe { library.get::<T>(name) }
        .map(|s| *s)
        .map_err(|e| format!("{}: {e}", String::from_utf8_lossy(name)))
}

impl Api {
    /// Laedt die Bibliotheken aus `dir`.
    ///
    /// # Errors
    ///
    /// Wenn eine Bibliothek oder ein Symbol fehlt.
    pub(crate) fn load(dir: &Path, accelerator: Accelerator) -> Result<&'static Self, String> {
        let core_path = dir.join("libtensorflowlite_jni.so");
        // SAFETY: eine unveraenderte TFLite-Bibliothek; ihre Initialisierer
        // haben keine Vorbedingungen. RTLD_GLOBAL, weil der GPU-Delegate
        // Symbole des Kerns erwarten kann.
        let core = unsafe { Library::open(Some(&core_path), RTLD_NOW | RTLD_GLOBAL) }
            .map_err(|e| format!("{}: {e}", core_path.display()))?;

        let gpu = match accelerator {
            Accelerator::Cpu => None,
            Accelerator::Gpu { .. } => {
                let path = dir.join("libtensorflowlite_gpu_jni.so");
                // SAFETY: wie oben.
                let library = unsafe { Library::open(Some(&path), RTLD_NOW) }
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                // SAFETY: Signaturen aus delegates/gpu/delegate.h (V2).
                unsafe {
                    Some(GpuApi {
                        options_default: symbol(&library, b"TfLiteGpuDelegateOptionsV2Default\0")?,
                        create: symbol(&library, b"TfLiteGpuDelegateV2Create\0")?,
                        delete: symbol(&library, b"TfLiteGpuDelegateV2Delete\0")?,
                        _library: library,
                    })
                }
            }
        };

        // SAFETY: Signaturen aus tensorflow/lite/core/c/c_api.h (2.16).
        let api = unsafe {
            Self {
                model_create_from_file: symbol(&core, b"TfLiteModelCreateFromFile\0")?,
                model_delete: symbol(&core, b"TfLiteModelDelete\0")?,
                options_create: symbol(&core, b"TfLiteInterpreterOptionsCreate\0")?,
                options_delete: symbol(&core, b"TfLiteInterpreterOptionsDelete\0")?,
                options_set_num_threads: symbol(&core, b"TfLiteInterpreterOptionsSetNumThreads\0")?,
                options_add_delegate: symbol(&core, b"TfLiteInterpreterOptionsAddDelegate\0")?,
                interpreter_create: symbol(&core, b"TfLiteInterpreterCreate\0")?,
                interpreter_delete: symbol(&core, b"TfLiteInterpreterDelete\0")?,
                allocate_tensors: symbol(&core, b"TfLiteInterpreterAllocateTensors\0")?,
                invoke: symbol(&core, b"TfLiteInterpreterInvoke\0")?,
                input_count: symbol(&core, b"TfLiteInterpreterGetInputTensorCount\0")?,
                input_tensor: symbol(&core, b"TfLiteInterpreterGetInputTensor\0")?,
                output_count: symbol(&core, b"TfLiteInterpreterGetOutputTensorCount\0")?,
                output_tensor: symbol(&core, b"TfLiteInterpreterGetOutputTensor\0")?,
                tensor_type: symbol(&core, b"TfLiteTensorType\0")?,
                tensor_num_dims: symbol(&core, b"TfLiteTensorNumDims\0")?,
                tensor_dim: symbol(&core, b"TfLiteTensorDim\0")?,
                tensor_byte_size: symbol(&core, b"TfLiteTensorByteSize\0")?,
                tensor_name: symbol(&core, b"TfLiteTensorName\0")?,
                copy_from_buffer: symbol(&core, b"TfLiteTensorCopyFromBuffer\0")?,
                copy_to_buffer: symbol(&core, b"TfLiteTensorCopyToBuffer\0")?,
                gpu,
                _core: core,
            }
        };
        Ok(Box::leak(Box::new(api)))
    }
}

/// Ein Tensor, wie ihn die Modellmetadaten beschreiben.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TensorInfo {
    pub(crate) name: String,
    pub(crate) datatype: &'static str,
    pub(crate) shape: Vec<i64>,
    pub(crate) byte_size: usize,
}

/// Ein- und Ausgaben eines Modells in Interpreterreihenfolge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ModelInfo {
    pub(crate) inputs: Vec<TensorInfo>,
    pub(crate) outputs: Vec<TensorInfo>,
}

/// Der OIP-Datentyp zu einem `TfLiteType` (`core/c/c_api_types.h`).
///
/// `None` fuer Typen, die OIP nicht kennt; ein solches Modell wird nicht
/// geladen, statt mit einem erfundenen Namen zu erscheinen.
pub(crate) const fn datatype(tflite_type: c_int) -> Option<&'static str> {
    match tflite_type {
        1 => Some("FP32"),
        2 => Some("INT32"),
        3 => Some("UINT8"),
        4 => Some("INT64"),
        5 => Some("BYTES"),
        6 => Some("BOOL"),
        7 => Some("INT16"),
        9 => Some("INT8"),
        10 => Some("FP16"),
        11 => Some("FP64"),
        13 => Some("UINT64"),
        16 => Some("UINT32"),
        17 => Some("UINT16"),
        _ => None,
    }
}

/// Ein Interpreter samt Modell, Optionen und Delegate.
///
/// Gehoert dem Thread, der ihn angelegt hat: der GL-Delegate bindet seinen
/// Kontext an diesen Thread. Deshalb weder `Send` noch `Sync` (rohe Zeiger).
pub(crate) struct Interpreter {
    api: &'static Api,
    model: *mut Opaque,
    options: *mut Opaque,
    delegate: *mut Opaque,
    /// Der eigentliche `TfLiteInterpreter`.
    raw: *mut Opaque,
}

impl Interpreter {
    /// Laedt ein Modell und legt seinen Interpreter an.
    ///
    /// # Errors
    ///
    /// Wenn Datei, Delegate, Interpreter oder Tensorbelegung scheitern, oder
    /// ein Tensor einen Typ hat, den OIP nicht kennt.
    pub(crate) fn load(
        api: &'static Api,
        path: &Path,
        accelerator: Accelerator,
        threads: i32,
    ) -> Result<(Self, ModelInfo), String> {
        let c_path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| format!("{}: Pfad mit Nullbyte", path.display()))?;

        // Ab hier haelt `this` jeden angelegten Zeiger; `Drop` raeumt auf,
        // auch wenn ein spaeterer Schritt scheitert.
        let mut this = Self {
            api,
            model: std::ptr::null_mut(),
            options: std::ptr::null_mut(),
            delegate: std::ptr::null_mut(),
            raw: std::ptr::null_mut(),
        };

        // SAFETY: gueltiger, nullterminierter Pfad.
        this.model = unsafe { (api.model_create_from_file)(c_path.as_ptr()) };
        if this.model.is_null() {
            return Err(format!("{}: kein gueltiges TFLite-Modell", path.display()));
        }
        // SAFETY: keine Vorbedingungen.
        this.options = unsafe { (api.options_create)() };
        if this.options.is_null() {
            return Err("Interpreteroptionen nicht angelegt".to_owned());
        }
        // SAFETY: `options` ist gueltig.
        unsafe { (api.options_set_num_threads)(this.options, threads) };

        if let Accelerator::Gpu { full_precision } = accelerator {
            let gpu = api
                .gpu
                .as_ref()
                .ok_or_else(|| "GPU-Bibliothek nicht geladen".to_owned())?;
            // SAFETY: gibt eine vollstaendig belegte Struktur zurueck (siehe
            // `GpuOptions` zur Groesse).
            let mut options = unsafe { (gpu.options_default)() };
            options.inference_preference = PREFERENCE_SUSTAINED_SPEED;
            if full_precision {
                options.is_precision_loss_allowed = 0;
                options.inference_priority1 = PRIORITY_MAX_PRECISION;
            } else {
                options.is_precision_loss_allowed = 1;
                options.inference_priority1 = PRIORITY_MIN_LATENCY;
            }
            options.inference_priority2 = PRIORITY_AUTO;
            options.inference_priority3 = PRIORITY_AUTO;
            options.experimental_flags = FLAG_ENABLE_QUANT | FLAG_GL_ONLY;
            // SAFETY: `options` ist eine gueltige, belegte Struktur.
            this.delegate = unsafe { (gpu.create)(&raw const options) };
            if this.delegate.is_null() {
                return Err("GPU-Delegate nicht angelegt (kein GLES 3.1?)".to_owned());
            }
            // SAFETY: beide gueltig; der Delegate lebt bis nach dem
            // Interpreter (Reihenfolge in `Drop`).
            unsafe { (api.options_add_delegate)(this.options, this.delegate) };
        }

        // SAFETY: Modell und Optionen gueltig. Ohne ausdruecklich erlaubten
        // Rueckfall liefert TFLite null, wenn der Delegate das Modell nicht
        // annimmt — genau das soll hier ein Fehler sein.
        this.raw = unsafe { (api.interpreter_create)(this.model, this.options) };
        if this.raw.is_null() {
            return Err(format!(
                "{}: Interpreter nicht angelegt{}",
                path.display(),
                if this.delegate.is_null() {
                    ""
                } else {
                    " — der GPU-Delegate hat das Modell abgelehnt (logcat: tflite)"
                }
            ));
        }
        // SAFETY: gueltiger Interpreter.
        if unsafe { (api.allocate_tensors)(this.raw) } != OK {
            return Err(format!("{}: Tensoren nicht belegbar", path.display()));
        }

        let info = this.info()?;
        Ok((this, info))
    }

    fn tensor_info(&self, tensor: *const Opaque) -> Result<TensorInfo, String> {
        if tensor.is_null() {
            return Err("Tensor fehlt".to_owned());
        }
        let api = self.api;
        // SAFETY: `tensor` ist ein gueltiger Tensor dieses Interpreters.
        let raw_name = unsafe { (api.tensor_name)(tensor) };
        let name = if raw_name.is_null() {
            String::new()
        } else {
            // SAFETY: nicht null, nullterminiert, lebt so lange wie der
            // Interpreter.
            unsafe { CStr::from_ptr(raw_name) }
                .to_string_lossy()
                .into_owned()
        };
        // SAFETY: wie oben.
        let raw_type = unsafe { (api.tensor_type)(tensor) };
        let datatype = datatype(raw_type)
            .ok_or_else(|| format!("{name}: TfLiteType {raw_type} ohne OIP-Namen"))?;
        // SAFETY: wie oben.
        let dims = unsafe { (api.tensor_num_dims)(tensor) };
        let mut shape = Vec::new();
        for i in 0..dims.max(0) {
            // SAFETY: `i` liegt unter `dims`.
            shape.push(i64::from(unsafe { (api.tensor_dim)(tensor, i) }));
        }
        // SAFETY: wie oben.
        let byte_size = unsafe { (api.tensor_byte_size)(tensor) };
        Ok(TensorInfo {
            name,
            datatype,
            shape,
            byte_size,
        })
    }

    fn info(&self) -> Result<ModelInfo, String> {
        let api = self.api;
        let mut info = ModelInfo::default();
        // SAFETY: gueltiger Interpreter.
        let inputs = unsafe { (api.input_count)(self.raw) };
        for i in 0..inputs.max(0) {
            // SAFETY: `i` liegt unter der Eingabezahl.
            let tensor = unsafe { (api.input_tensor)(self.raw, i) };
            info.inputs.push(self.tensor_info(tensor)?);
        }
        // SAFETY: gueltiger Interpreter.
        let outputs = unsafe { (api.output_count)(self.raw) };
        for i in 0..outputs.max(0) {
            // SAFETY: `i` liegt unter der Ausgabezahl.
            let tensor = unsafe { (api.output_tensor)(self.raw, i) };
            info.outputs.push(self.tensor_info(tensor)?);
        }
        Ok(info)
    }

    /// Eine Inferenz: Eingaben in Interpreterreihenfolge hinein, alle
    /// Ausgaben heraus.
    ///
    /// # Errors
    ///
    /// Bei falscher Eingabezahl oder -groesse oder wenn `Invoke` scheitert.
    pub(crate) fn run(&mut self, inputs: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, String> {
        let api = self.api;
        // SAFETY: gueltiger Interpreter.
        let count = unsafe { (api.input_count)(self.raw) };
        if usize::try_from(count).ok() != Some(inputs.len()) {
            return Err(format!(
                "{count} Eingaben erwartet, {} erhalten",
                inputs.len()
            ));
        }
        for (i, bytes) in (0_i32..).zip(inputs) {
            // SAFETY: `i` liegt unter der Eingabezahl.
            let tensor = unsafe { (api.input_tensor)(self.raw, i) };
            // SAFETY: gueltiger Tensor; TFLite prueft die Groesse und
            // kopiert genau `len` Bytes aus einem gueltigen Puffer.
            let status =
                unsafe { (api.copy_from_buffer)(tensor, bytes.as_ptr().cast(), bytes.len()) };
            if status != OK {
                return Err(format!("Eingabe {i}: {} Bytes passen nicht", bytes.len()));
            }
        }
        // SAFETY: gueltiger Interpreter mit belegten Tensoren.
        if unsafe { (api.invoke)(self.raw) } != OK {
            return Err("Invoke gescheitert".to_owned());
        }
        // SAFETY: gueltiger Interpreter.
        let outputs = unsafe { (api.output_count)(self.raw) };
        let mut result = Vec::new();
        for i in 0..outputs.max(0) {
            // SAFETY: `i` liegt unter der Ausgabezahl.
            let tensor = unsafe { (api.output_tensor)(self.raw, i) };
            // SAFETY: gueltiger Tensor.
            let size = unsafe { (api.tensor_byte_size)(tensor) };
            let mut buffer = vec![0_u8; size];
            // SAFETY: der Puffer fasst genau `size` Bytes.
            let status = unsafe { (api.copy_to_buffer)(tensor, buffer.as_mut_ptr().cast(), size) };
            if status != OK {
                return Err(format!("Ausgabe {i} nicht lesbar"));
            }
            result.push(buffer);
        }
        Ok(result)
    }
}

impl Drop for Interpreter {
    fn drop(&mut self) {
        let api = self.api;
        // SAFETY: jeder Zeiger ist null oder von der passenden Create-Funktion
        // und wird genau einmal freigegeben. Der Interpreter geht vor dem
        // Delegate, wie TFLite es verlangt.
        unsafe {
            if !self.raw.is_null() {
                (api.interpreter_delete)(self.raw);
            }
            if !self.delegate.is_null()
                && let Some(gpu) = api.gpu.as_ref()
            {
                (gpu.delete)(self.delegate);
            }
            if !self.options.is_null() {
                (api.options_delete)(self.options);
            }
            if !self.model.is_null() {
                (api.model_delete)(self.model);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_numeric_tflite_type_has_its_oip_name() {
        assert_eq!(datatype(1), Some("FP32"));
        assert_eq!(datatype(3), Some("UINT8"));
        assert_eq!(datatype(9), Some("INT8"));
        assert_eq!(datatype(10), Some("FP16"));
        assert_eq!(datatype(2), Some("INT32"));
    }

    #[test]
    fn a_type_oip_does_not_know_has_no_name() {
        // Komplexe Zahlen, Ressourcen, Varianten, INT4: lieber nicht laden
        // als einen erfundenen Namen melden.
        for unknown in [0, 8, 12, 14, 15, 18, 99] {
            assert_eq!(datatype(unknown), None, "TfLiteType {unknown}");
        }
    }

    #[test]
    fn the_gpu_options_head_matches_the_c_layout() {
        // Die Felder, die gesetzt werden, muessen an den Offsets der
        // C-Struktur liegen (5 x i32, dann i64 auf 8 ausgerichtet).
        assert_eq!(std::mem::offset_of!(GpuOptions, inference_priority3), 16);
        assert_eq!(std::mem::offset_of!(GpuOptions, experimental_flags), 24);
        assert_eq!(
            std::mem::offset_of!(GpuOptions, max_delegated_partitions),
            32
        );
        assert_eq!(std::mem::offset_of!(GpuOptions, serialization_dir), 40);
        assert_eq!(std::mem::offset_of!(GpuOptions, model_token), 48);
    }

    #[test]
    fn a_missing_library_is_an_error_not_a_crash() {
        let error = Api::load(Path::new("/nonexistent"), Accelerator::Cpu)
            .err()
            .unwrap_or_default();
        assert!(error.contains("libtensorflowlite_jni.so"), "{error}");
    }
}
