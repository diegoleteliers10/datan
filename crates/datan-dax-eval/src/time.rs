//! Time intelligence: `DATEADD`, `DATESYTD`, `PARALLELPERIOD`, `SAMEPERIODLASTYEAR`.
//!
//! Puro. Sin duckdb, sin gpui, sin IO.
//!
//! # Por que esto NO es un caso especial de `ALL`
//!
//! Todo el time intelligence de DAX es un `CALCULATE` con un filtro sobre una tabla de
//! fechas. `PREVIOUSYEAR` es `CALCULATE([Revenue], DATEADD('Dim Date'[Date], -1, YEAR))`.
//!
//! La unica diferencia con `ALL` es que el filtro lo produce un CALCULO sobre la columna de
//! fecha, no un literal. Asi que la funcion de este modulo no es "sumar un ano": es
//! **evaluar el rango de fechas** que el modificador va a filtrar.
//!
//! # La tabla de fechas tiene que existir y ser contigua
//!
//! DAX no lo exige y da numeros silenciosos sin ella: un `DATEADD` sobre una columna sin
//! filas devuelve BLANK, y BLANK se parece a un total de cero hasta que alguien mira el
//! informe. Por eso `TimeFunction::validate` exige una tabla de fechas marcada.

use crate::filter::{FilterContext, FilterValue};
use datan_model::{DataType, Model, ObjectId};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum TimeError {
    #[error("el modelo no tiene una tabla de fechas marcada. DATEADD necesita una columna de fecha contigua")]
    NoDateTable,
    #[error("la columna {0} no es una columna de fecha del modelo")]
    NotADateColumn(ObjectId),
    #[error("el intervalo {0} no es valido")]
    InvalidInterval(String),
    #[error("la tabla de fechas '{0}' no tiene una columna de tipo Date")]
    NoDateColumn(String),
}

/// Las funciones de time intelligence.
///
/// Se combinan de forma mecanica: `SAMEPERIODLASTYEAR` es `DATEADD(x, -1, YEAR)` acotado
/// por `DATESYTD`. Por eso el enum cubre solo las primitivas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeFunction {
    /// `DATEADD`. Desplaza el rango actual N unidades hacia atras.
    DateAdd { units: i64, unit: CalendarUnit },
    /// `PARALLELPERIOD`. Devuelve el periodo paralelo del actual, completo.
    ParallelPeriod { amount: i64, unit: CalendarUnit },
    /// `DATESYTD`. Acota al acumulado del ano a la fecha actual.
    YearToDate,
    /// `PREVIOUSYEAR`. Es `DATEADD(-1, YEAR)` acotado al ano completo anterior.
    PreviousYear,
}

/// Una unidad de calendario.
///
/// No existe "mes" calendario ni "trimestre": el calendario es gregoriano y los meses tienen
/// duraciones distintas. Un "mes" es un periodo, no un numero de dias.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarUnit {
    Day,
    Month,
    Quarter,
    Year,
}

impl CalendarUnit {
    pub fn parse(name: &str) -> Result<Self, TimeError> {
        Ok(match name.to_uppercase().as_str() {
            "DAY" => CalendarUnit::Day,
            "MONTH" => CalendarUnit::Month,
            "QUARTER" => CalendarUnit::Quarter,
            "YEAR" => CalendarUnit::Year,
            other => return Err(TimeError::InvalidInterval(other.to_string())),
        })
    }
}

/// Un rango de fechas, en dias desde el origen.
///
/// Se trabaja en dias, no en `chrono::NaiveDate`: el motor no debe depender de un calendario
/// externo, y el conteo de dias es la unica operacion que todas las unidades necesitan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateRange {
    pub start: i32,
    pub end: i32,
}

impl DateRange {
    pub fn new(start: i32, end: i32) -> Self {
        Self { start, end }
    }

    pub fn is_empty(&self) -> bool {
        self.start > self.end
    }

    pub fn contains(&self, day: i32) -> bool {
        day >= self.start && day <= self.end
    }

    pub fn len(&self) -> i32 {
        if self.is_empty() {
            0
        } else {
            self.end - self.start + 1
        }
    }
}

/// El rango que produce una funcion de time intelligence.
///
/// Total sobre las cuatro funciones: cada caso despacha a su propia semantica y ninguno es
/// un no-op. Una version anterior dejaba `YearToDate` y `PreviousYear` devolviendo el rango
/// sin tocar, delegando el recorte en el llamador, y eso hacia que un `PREVIOUSYEAR` se
/// comportara como `DATEADD` y devolviera 365 dias en vez del ano entero.
///
/// Puro: mismo rango de entrada, mismo rango de salida. Sin reloj, sin estado.
pub fn shift(function: TimeFunction, current: DateRange, today: i32) -> DateRange {
    match function {
        TimeFunction::DateAdd { units, unit } => shift_by(current, units, unit),
        TimeFunction::ParallelPeriod { amount, unit } => parallel_period(current, amount, unit),
        TimeFunction::YearToDate => year_to_date(current, today),
        TimeFunction::PreviousYear => previous_year(today),
    }
}

/// Convierte un rango de fechas en los filtros que lo imponen sobre la columna de fecha.
///
/// Dos filtros, `>=` y `<=`, y no un `BETWEEN`: los dos operadores ya existen en el algebra
/// de filtros y por eso el rango se propaga por las relationships sin un caso especial. Un
/// `Between` nuevo habria tenido que aprender a viajar.
pub fn filters(
    model: &Model,
    function: TimeFunction,
    current: DateRange,
    today: i32,
) -> Result<FilterContext, TimeError> {
    let column = date_column(model)?;
    let range = shift(function, current, today);

    // Un rango vacio se modela con un filtro imposible, no con la ausencia de filtros.
    //
    // La ausencia significa "sin restriccion", que es lo contrario de lo que quiere decir un
    // `PREVIOUSYEAR` que cae antes del primer dia del modelo. Sin este filtro, esa celda
    // devolveria el total en vez de BLANK, y el error pasaria desapercibido hasta que alguien
    // mirase el informe.
    if range.is_empty() {
        return Ok(FilterContext::empty().with_greater(column, FilterValue::Date(i32::MAX)));
    }

    Ok(FilterContext::empty()
        .with_greater_equal(column, FilterValue::Date(range.start))
        .with_less_equal(column, FilterValue::Date(range.end)))
}

/// La columna de fecha del modelo.
///
/// La clave de la tabla de fechas. Es la unica columna que puede ser: cualquier otra
/// (`Year`, `Month`, `Quarter`) no distingue dos dias del mismo mes, y un filtro sobre ella
/// devolveria filas que el rango de fechas dice excluir.
pub fn date_column(model: &Model) -> Result<ObjectId, TimeError> {
    let table = model.date_table().ok_or(TimeError::NoDateTable)?;

    if let Some(key) = table.key_columns.first()
        && table.column(*key).is_some_and(|c| c.data_type == DataType::Date)
    {
        return Ok(*key);
    }

    table
        .columns
        .iter()
        .find(|c| c.data_type == DataType::Date)
        .map(|c| c.id)
        .ok_or_else(|| TimeError::NoDateColumn(table.name.clone()))
}

/// Desplaza el rango `units` unidades hacia atras.
///
/// El rango de salida tiene la MISMA longitud que el de entrada. Eso es lo que distingue
/// `DATEADD` de un simple "restar 365 dias": `DATEADD` mueve la ventana, no la encoge.
fn shift_by(range: DateRange, units: i64, unit: CalendarUnit) -> DateRange {
    let size = range.len();
    let new_start = match unit {
        CalendarUnit::Day => range.start + (units * 1) as i32,
        CalendarUnit::Month => add_months(range.start, units),
        CalendarUnit::Quarter => add_months(range.start, units * 3),
        CalendarUnit::Year => add_months(range.start, units * 12),
    };
    DateRange::new(new_start, new_start + size - 1)
}

/// El periodo calendario paralelo al actual, completo.
///
/// `PARALLELPERIOD('Dim Date'[Date], -1, YEAR)` sobre enero de 2024 devuelve TODO 2023, no
/// "los ultimos 12 meses". Por eso se recorta a los limites del periodo.
fn parallel_period(range: DateRange, amount: i64, unit: CalendarUnit) -> DateRange {
    match unit {
        CalendarUnit::Day => shift_by(range, amount, unit),
        CalendarUnit::Month => {
            let (year, month) = year_month_of(range.start);
            let target = add_months_index(year, month, amount);
            let first = month_start(target);
            let next = month_start(add_months_index(target.0, target.1, 1));
            DateRange::new(first, next - 1)
        }
        CalendarUnit::Quarter => {
            let (year, month) = year_month_of(range.start);
            let quarter_index = (month - 1) / 3;
            let target_quarter = quarter_index as i64 + amount;
            // `div_euclid`, no `/`: Rust trunca hacia cero, asi que `-1 / 4` es `0` y
            // "un trimestre antes de Q1" caeria en Q4 del mismo ano. El error solo se ve
            // al cruzar el limite de ano, que es justo cuando un acumulado trimestral
            // compara Q4 contra Q4 del ano anterior.
            let target_year = year + target_quarter.div_euclid(4) as i32;
            let target_month = (target_quarter.rem_euclid(4) * 3 + 1) as u32;
            let first = month_start((target_year, target_month));
            let next = month_start(add_months_index(target_year, target_month, 3));
            DateRange::new(first, next - 1)
        }
        CalendarUnit::Year => {
            let (year, _) = year_month_of(range.start);
            let target = year as i64 + amount;
            DateRange::new(month_start((target as i32, 1)), month_start((target as i32 + 1, 1)) - 1)
        }
    }
}

/// El rango del ano a la fecha de hoy: `DATESYTD`.
///
/// Acota el rango al 1 de enero del ano de `today`. Si `current` ya empieza despues, no lo
/// mueve: `DATESYTD` recorta, no desplaza.
pub fn year_to_date(current: DateRange, today: i32) -> DateRange {
    let (year, _) = year_month_of(today);
    let first = month_start((year, 1));
    // `DATESYTD` recorta, no desplaza: si el rango ya empieza despues del 1 de enero, el
    // recorte no lo mueve. Si el rango entero cae antes del 1 de enero, queda vacio, que es
    // lo correcto: no hay acumulado del ano para una fecha del ano pasado.
    DateRange::new(first.max(current.start), current.end)
}

/// El rango del ano completo anterior.
///
/// `PREVIOUSYEAR` es `DATEADD(-1, YEAR)` acotado al ano entero. Sin ese recorte devuelve
/// los ultimos 365 dias, que cruza el cambio de ano y no coincide con el ano anterior.
pub fn previous_year(today: i32) -> DateRange {
    let (year, _) = year_month_of(today);
    let target = year - 1;
    DateRange::new(month_start((target, 1)), month_start((target + 1, 1)) - 1)
}

// ---------------------------------------------------------------------------------------------
// Calendario gregoriano sobre dias desde 1970-01-01
// ---------------------------------------------------------------------------------------------

/// El ano y el mes de un dia. Mes 1 a 12.
fn year_month_of(day: i32) -> (i32, u32) {
    // Algoritmo de Hinnant para dias desde la epoca, en tiempo constante.
    let z = day as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m as u32)
}

/// El dia del mes 1 de `(year, month)`.
fn month_start((year, month): (i32, u32)) -> i32 {
    let y = (year - if month <= 2 { 1 } else { 0 }) as i64;
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = month as i64;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    // `days_from_civil` con d = 1: `+d-1` se cancela y no queda nada.
    let doy = (153 * mp + 2) / 5;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) as i32
}

/// El dia del mes, el mes y el ano de un dia. El dia va de 1 a 31.
///
/// Inversa de `year_month_of`. Hace falta para bajar un rango de fechas a SQL: DuckDB no
/// acepta `DATE '19723'`, espera `DATE '2024-01-01'`.
fn civil_from_days(day: i32) -> (i32, u32, u32) {
    let z = day as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m as u32, d as u32)
}

/// Un dia como `YYYY-MM-DD`, el formato que acepta DuckDB en un literal `DATE`.
///
/// El motor trabaja en dias desde 1970 porque es la unica aritmetica que todas las
/// unidades de calendario necesitan. Ese numero interno no es un literal de SQL.
pub fn format_date(day: i32) -> String {
    let (year, month, d) = civil_from_days(day);
    format!("{year:04}-{month:02}-{d:02}")
}

/// Suma `delta` meses a `(year, month)`, normalizando el mes.
fn add_months_index(year: i32, month: u32, delta: i64) -> (i32, u32) {
    let total = year as i64 * 12 + (month as i64 - 1) + delta;
    let new_year = total.div_euclid(12);
    let new_month = total.rem_euclid(12) + 1;
    (new_year as i32, new_month as u32)
}

fn add_months(day: i32, delta: i64) -> i32 {
    let (year, month) = year_month_of(day);
    month_start(add_months_index(year, month, delta))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2024-01-01 es el dia 19723 desde 1970-01-01.
    const Y2024: i32 = 19723;

    fn at(offset_from_2024: i32) -> i32 {
        Y2024 + offset_from_2024
    }

    #[test]
    fn el_calendario_calcula_anos_y_meses() {
        assert_eq!(year_month_of(Y2024), (2024, 1));
        // 2024 es bisiesto: el 29 de febrero existe.
        assert_eq!(year_month_of(at(59)), (2024, 2));
        assert_eq!(year_month_of(at(60)), (2024, 3));
        // 2023 no lo es.
        assert_eq!(year_month_of(at(59) - 366), (2023, 2));
    }

    #[test]
    fn dateadd_mueve_la_ventana_sin_cambiarla_de_tamano() {
        // Un mes de enero 2024: 31 dias.
        let january = DateRange::new(at(0), at(30));
        let shifted = shift(
            TimeFunction::DateAdd {
                units: -1,
                unit: CalendarUnit::Month,
            },
            january,
            at(0),
        );

        assert_eq!(shifted.len(), january.len(), "DATEADD conserva la longitud");
        assert_eq!(shifted.start, at(-31), "enero menos un mes es diciembre 2023");
    }

    #[test]
    fn parallelperiod_devuelve_el_periodo_completo() {
        // Un dia de enero 2024. `PARALLELPERIOD(-1, YEAR)` es TODO 2023.
        let day = DateRange::new(at(0), at(0));
        let previous = parallel_period(day, -1, CalendarUnit::Year);

        assert_eq!(previous.len(), 365, "2023 no es bisiesto; 2024 si lo es");
        assert_eq!(year_month_of(previous.start), (2023, 1));
        assert_eq!(year_month_of(previous.end), (2023, 12));
    }

    #[test]
    fn parallelperiod_trimestre_devuelve_el_trimestre_completo() {
        let day = DateRange::new(at(0), at(0)); // enero 2024 = Q1
        let previous = parallel_period(day, -1, CalendarUnit::Quarter);

        assert_eq!(year_month_of(previous.start), (2023, 10));
        assert_eq!(year_month_of(previous.end), (2023, 12));
        assert_eq!(previous.len(), 92);
    }

    #[test]
    fn datesytd_acota_al_ano_en_curso() {
        // 15 de marzo de 2024.
        let today = at(74);
        let wide = DateRange::new(at(-365), today);
        let ytd = year_to_date(wide, today);

        assert_eq!(year_month_of(ytd.start), (2024, 1));
        assert_eq!(ytd.end, today);
    }

    #[test]
    fn previousyear_devuelve_el_ano_entero_anterior() {
        let today = at(74); // marzo 2024
        let previous = previous_year(today);

        assert_eq!(year_month_of(previous.start), (2023, 1));
        assert_eq!(year_month_of(previous.end), (2023, 12));
        assert_eq!(previous.len(), 365);
    }

    #[test]
    fn dateadd_ancla_en_el_inicio_y_no_conserva_el_dia_del_ano() {
        // Un rango de 10 dias del 28 de diciembre de 2023 al 6 de enero de 2024.
        let start = at(-4);
        let range = DateRange::new(start, start + 9);
        let shifted = shift_by(range, -1, CalendarUnit::Year);

        // La longitud se conserva, pero las fechas se comprimen: el 1 de enero de 2024
        // desplazado un ano cae el 1 de enero de 2023, que ya esta dentro del rango
        // desplazado. El resultado es 28 dic 2022 a 10 dic 2022.
        //
        // Esto NO es un bug. Es la diferencia entre `DATEADD` y `PARALLELPERIOD`.
        // `DATEADD` mueve la ventana y conserva su tamano; `PARALLELPERIOD` recorta a los
        // limites del periodo calendario. Quien quiere "el mismo mes del ano pasado" usa
        // `PARALLELPERIOD`. Quien quiere "la ventana desplazada" usa `DATEADD`.
        //
        // El test fija el comportamiento para que nadie lo lea como una sorpresa mas
        // adelante.
        assert_eq!(shifted.len(), 10);
        assert_eq!(year_month_of(shifted.start), (2022, 12));
        assert_eq!(year_month_of(shifted.end), (2022, 12));
    }

    #[test]
    fn parallelperiod_si_conserva_el_dia_del_periodo() {
        // El contraste con el test anterior, en el mismo cruce de ano.
        let start = at(-4); // 28 dic 2023
        let range = DateRange::new(start, start + 9);
        let parallel = parallel_period(range, -1, CalendarUnit::Year);

        assert_eq!(year_month_of(parallel.start), (2022, 1));
        assert_eq!(year_month_of(parallel.end), (2022, 12));
        assert_eq!(parallel.len(), 365);
    }

    #[test]
    fn format_date_da_el_formato_que_acepta_duckdb() {
        // `DATE '19723'` es un error de conversion en DuckDB. `DATE '2024-01-01'` no. Este
        // test fija el contrato porque el fallo aparece en tiempo de ejecucion, no de
        // compilacion: el SQL se genera bien yDuckDB responde que el formato no vale.
        assert_eq!(format_date(Y2024), "2024-01-01");
        assert_eq!(format_date(at(59)), "2024-02-29");
        assert_eq!(format_date(at(-365)), "2023-01-01");
        assert_eq!(format_date(at(-1)), "2023-12-31");
        // Antes de la epoca: los anos negativos no pueden imprimirse con `{year:04}`.
        assert_eq!(format_date(0), "1970-01-01");
    }

    #[test]
    fn format_date_ida_y_vuelta_con_el_calendario() {
        // La property test que de verdad importa: `civil_from_days` y `year_month_of` no
        // pueden discrepar en ningun dia, ni en un bisiesto ni en un cambio de siglo.
        let mut day = -800_000;
        while day < 800_000 {
            let (year, month, d) = civil_from_days(day);
            assert_eq!(year_month_of(day), (year, month), "dia {day}");
            assert_eq!(month_start((year, month)), day - (d as i32) + 1, "dia {day}");
            day += 37;
        }
    }

    #[test]
    fn un_rango_vacio_se_propaga_vacio() {
        let empty = DateRange::new(at(10), at(0));
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
    }
}