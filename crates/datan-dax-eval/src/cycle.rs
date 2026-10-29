//! Deteccion de ciclos bidireccionales en el grafo de relaciones.
//!
//! Que la propagacion de filtros CONVERJA en un ciclo (ver `tests/cycles.rs`) no significa
//! que el ciclo se pueda escribir en SQL. Un `JOIN` exige un arbol de joins aciclico, y un
//! grafo con filtro bidireccional no lo tiene.
//!
//! Por eso esta deteccion existe igual: para RECHAZAR el fast path con un mensaje claro, no
//! para evitar el calculo. El calculo lo hace el interprete, que si lo resuelve.

use datan_model::{FilterDirection, Model, ObjectId, RelationshipRole};
use std::collections::HashMap;

/// Detecta un ciclo en el grafo de relaciones, siguiendo la direccion del filtro.
///
/// DFS con marcado de tres colores: blanco sin visitar, gris en la pila, negro terminado.
/// Encontrar un nodo gris es encontrar un ciclo.
///
/// Trabaja sobre COLUMNAS, no sobre tablas, porque es lo que la propagacion restringe. Las
/// columnas de una relationship son distintas, asi que un grafo aciclico de tablas puede
/// parecer ciclico si se colapsa: `Bridge[ProductKey]` aparece en las dos relaciones del
/// puente, y con `Single` en una de ellas el camino se cierra.
pub fn has_bidirectional_cycle(model: &Model) -> bool {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        White,
        Grey,
        Black,
    }

    // Aristas: de la columna del origen a la del destino, segun la direccion del filtro.
    let mut edges: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    for relationship in &model.relationships {
        if relationship.role != RelationshipRole::Active
            || relationship.direction == FilterDirection::None
        {
            continue;
        }
        edges
            .entry(relationship.from_column)
            .or_default()
            .push(relationship.to_column);
        if relationship.direction == FilterDirection::Both {
            edges
                .entry(relationship.to_column)
                .or_default()
                .push(relationship.from_column);
        }
    }

    fn visit(
        node: ObjectId,
        edges: &HashMap<ObjectId, Vec<ObjectId>>,
        marks: &mut HashMap<ObjectId, Mark>,
    ) -> bool {
        match marks.get(&node).copied().unwrap_or(Mark::White) {
            Mark::Black => return false,
            Mark::Grey => return true,
            Mark::White => {}
        }
        marks.insert(node, Mark::Grey);
        for next in edges.get(&node).into_iter().flatten() {
            if visit(*next, edges, marks) {
                return true;
            }
        }
        marks.insert(node, Mark::Black);
        false
    }

    let nodes: Vec<ObjectId> = edges.keys().copied().collect();
    // El marcado se reinicia por raiz. Compartirlo entre raices hacia que una raiz ya
    // terminada (negra) se confundiera con otra en curso (gris) cuando comparten columna.
    for node in nodes {
        let mut marks = HashMap::new();
        if visit(node, &edges, &mut marks) {
            return true;
        }
    }
    false
}
