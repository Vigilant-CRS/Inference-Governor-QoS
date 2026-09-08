//! Vektor fester Hoechstkapazitaet ohne Heap-Allokation.
//!
//! Spec L-003 verlangt, dass jede Queue eine explizite maximale Kapazitaet
//! besitzt; Spec 8.1 verlangt, im Scheduling-Entscheidungspfad soweit praktisch
//! erreichbar nicht zu allokieren; Spec 26.4 verbietet unbeschraenkte Aufnahme
//! aus fremd kontrollierten Groessen.
//!
//! Ein `Vec` erfuellt keine dieser drei Anforderungen von selbst — er waechst,
//! wenn man ihn laesst. Dieser Typ **kann** nicht wachsen: die Kapazitaet steht
//! im Typ, ein Ueberlauf ist ein sichtbarer Rueckgabewert und kein
//! Reallokationsaufruf. Damit ist die Speicherobergrenze des Schedulers zur
//! Kompilierzeit ablesbar.
//!
//! Bewusst kein externes Crate: der Kern bleibt dependencyfrei (Spec 20,
//! `cargo deny`-Flaeche), und die Implementierung ist klein genug, um sie
//! vollstaendig zu pruefen.

use core::fmt;

/// Ein Vektor mit hoechstens `N` Elementen, ohne Heap-Allokation.
pub struct ArrayVec<T, const N: usize> {
    items: [Option<T>; N],
    len: usize,
}

/// Fehler beim Einfuegen in einen vollen [`ArrayVec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityExceeded;

impl fmt::Display for CapacityExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Kapazitaet erschoepft")
    }
}

impl<T, const N: usize> ArrayVec<T, N> {
    /// Ein leerer Vektor.
    #[must_use]
    pub fn new() -> Self {
        Self {
            items: [const { None }; N],
            len: 0,
        }
    }

    /// Die Anzahl enthaltener Elemente.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Wahr, wenn kein Element enthalten ist.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Wahr, wenn keine weiteren Elemente aufgenommen werden koennen.
    #[must_use]
    pub const fn is_full(&self) -> bool {
        self.len >= N
    }

    /// Die Hoechstkapazitaet.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Haengt ein Element an.
    ///
    /// # Errors
    ///
    /// [`CapacityExceeded`], wenn der Vektor voll ist. Der Aufrufer muss den
    /// Fall behandeln — ein stiller Drop waere genau das, was Spec 11.4
    /// verbietet.
    pub fn push(&mut self, value: T) -> Result<(), CapacityExceeded> {
        let slot = self.items.get_mut(self.len).ok_or(CapacityExceeded)?;
        *slot = Some(value);
        self.len = self.len.saturating_add(1);
        Ok(())
    }

    /// Fuegt ein Element an `index` ein und verschiebt die folgenden nach
    /// hinten.
    ///
    /// Ein `index` hinter dem Ende wird wie ein [`Self::push`] behandelt.
    ///
    /// # Errors
    ///
    /// [`CapacityExceeded`], wenn der Vektor voll ist.
    pub fn insert(&mut self, index: usize, value: T) -> Result<(), CapacityExceeded> {
        if self.len >= self.items.len() {
            return Err(CapacityExceeded);
        }
        let at = index.min(self.len);
        let mut i = self.len;
        while i > at {
            let previous = self
                .items
                .get_mut(i.saturating_sub(1))
                .ok_or(CapacityExceeded)?
                .take();
            *self.items.get_mut(i).ok_or(CapacityExceeded)? = previous;
            i = i.saturating_sub(1);
        }
        *self.items.get_mut(at).ok_or(CapacityExceeded)? = Some(value);
        self.len = self.len.saturating_add(1);
        Ok(())
    }

    /// Entfernt das Element an `index` und verschiebt die folgenden nach vorn.
    ///
    /// Reihenfolgeerhaltend, weil FIFO-Queues genau darauf angewiesen sind
    /// (Spec G-003). Gibt `None` bei ungueltigem Index zurueck.
    pub fn remove(&mut self, index: usize) -> Option<T> {
        if index >= self.len {
            return None;
        }
        let taken = self.items.get_mut(index)?.take();
        let mut i = index;
        while i.saturating_add(1) < self.len {
            let next = self.items.get_mut(i.saturating_add(1))?.take();
            *self.items.get_mut(i)? = next;
            i = i.saturating_add(1);
        }
        self.len = self.len.saturating_sub(1);
        taken
    }

    /// Entfernt das erste Element.
    pub fn pop_front(&mut self) -> Option<T> {
        self.remove(0)
    }

    /// Eine Referenz auf das Element an `index`.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            return None;
        }
        self.items.get(index)?.as_ref()
    }

    /// Eine veraenderliche Referenz auf das Element an `index`.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        if index >= self.len {
            return None;
        }
        self.items.get_mut(index)?.as_mut()
    }

    /// Entfernt alle Elemente.
    pub fn clear(&mut self) {
        for slot in &mut self.items {
            *slot = None;
        }
        self.len = 0;
    }

    /// Iteriert ueber die enthaltenen Elemente.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.items.iter().take(self.len).filter_map(Option::as_ref)
    }

    /// Iteriert veraenderlich ueber die enthaltenen Elemente.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.items
            .iter_mut()
            .take(self.len)
            .filter_map(Option::as_mut)
    }

    /// Entfernt alle Elemente, fuer die `pred` falsch ist, und uebergibt sie an
    /// `on_removed`.
    ///
    /// Die entfernten Elemente werden **nicht** verworfen, sondern
    /// weitergereicht: jeder aus einer Queue entfernte Request braucht einen
    /// expliziten terminalen Zustand (Spec 8.2).
    pub fn retain_reporting<F, G>(&mut self, mut pred: F, mut on_removed: G)
    where
        F: FnMut(&T) -> bool,
        G: FnMut(T),
    {
        let mut i = 0;
        while i < self.len {
            let keep = self.get(i).is_some_and(&mut pred);
            if keep {
                i = i.saturating_add(1);
            } else if let Some(removed) = self.remove(i) {
                on_removed(removed);
            } else {
                break;
            }
        }
    }
}

impl<T, const N: usize> Default for ArrayVec<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: fmt::Debug, const N: usize> fmt::Debug for ArrayVec<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T: Clone, const N: usize> Clone for ArrayVec<T, N> {
    fn clone(&self) -> Self {
        let mut out = Self::new();
        for item in self.iter() {
            // Kann nicht fehlschlagen: gleiche Kapazitaet, gleiche Laenge.
            if out.push(item.clone()).is_err() {
                break;
            }
        }
        out
    }
}

impl<T: PartialEq, const N: usize> PartialEq for ArrayVec<T, N> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().zip(other.iter()).all(|(a, b)| a == b)
    }
}

impl<T: Eq, const N: usize> Eq for ArrayVec<T, N> {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn push_stops_at_capacity_instead_of_growing() {
        let mut v: ArrayVec<u32, 3> = ArrayVec::new();
        assert!(v.push(1).is_ok());
        assert!(v.push(2).is_ok());
        assert!(v.push(3).is_ok());
        assert!(v.is_full());
        assert_eq!(
            v.push(4),
            Err(CapacityExceeded),
            "der Ueberlauf ist sichtbar"
        );
        assert_eq!(v.len(), 3, "und veraendert den Inhalt nicht");
    }

    #[test]
    fn insert_shifts_the_tail_and_preserves_order() {
        let mut v: ArrayVec<u32, 5> = ArrayVec::new();
        for i in [1_u32, 3, 5] {
            v.push(i).unwrap();
        }
        // Vorn, in der Mitte — und der Rest rueckt jeweils nach hinten.
        v.insert(0, 0).unwrap();
        v.insert(2, 2).unwrap();
        let got: Vec<u32> = v.iter().copied().collect();
        assert_eq!(got, vec![0, 1, 2, 3, 5]);
        assert_eq!(v.len(), 5);
    }

    /// Ein Index hinter dem Ende haengt an, statt eine Luecke zu lassen.
    #[test]
    fn insert_beyond_the_end_appends() {
        let mut v: ArrayVec<u32, 4> = ArrayVec::new();
        v.push(1).unwrap();
        v.insert(99, 2).unwrap();
        let got: Vec<u32> = v.iter().copied().collect();
        assert_eq!(got, vec![1, 2]);
    }

    /// Wie [`ArrayVec::push`]: der Ueberlauf ist sichtbar und veraendert den
    /// Inhalt nicht. Ein stiller Drop waere hier besonders teuer — der
    /// Look-ahead wuerde dann eine erwartete geschuetzte Ankunft uebersehen.
    #[test]
    fn insert_stops_at_capacity_instead_of_growing() {
        let mut v: ArrayVec<u32, 2> = ArrayVec::new();
        v.push(1).unwrap();
        v.push(3).unwrap();
        assert_eq!(v.insert(1, 2), Err(CapacityExceeded));
        let got: Vec<u32> = v.iter().copied().collect();
        assert_eq!(got, vec![1, 3], "der Inhalt bleibt unveraendert");
    }

    #[test]
    fn remove_preserves_order() {
        let mut v: ArrayVec<u32, 5> = ArrayVec::new();
        for i in 1..=5 {
            v.push(i).unwrap();
        }
        assert_eq!(v.remove(1), Some(2));
        let got: Vec<u32> = v.iter().copied().collect();
        assert_eq!(got, vec![1, 3, 4, 5]);
    }

    #[test]
    fn out_of_range_access_returns_none_instead_of_panicking() {
        let mut v: ArrayVec<u32, 4> = ArrayVec::new();
        v.push(7).unwrap();
        assert_eq!(v.get(0), Some(&7));
        assert_eq!(v.get(1), None);
        assert_eq!(v.remove(9), None);
        assert_eq!(v.get_mut(3), None);
    }

    #[test]
    fn retain_reports_every_removed_element() {
        let mut v: ArrayVec<u32, 8> = ArrayVec::new();
        for i in 0..8 {
            v.push(i).unwrap();
        }
        let mut removed = Vec::new();
        v.retain_reporting(|x| x % 2 == 0, |x| removed.push(x));

        let kept: Vec<u32> = v.iter().copied().collect();
        assert_eq!(kept, vec![0, 2, 4, 6]);
        assert_eq!(
            removed,
            vec![1, 3, 5, 7],
            "kein Element verschwindet unbemerkt"
        );
    }

    #[test]
    fn pop_front_is_fifo() {
        let mut v: ArrayVec<u32, 4> = ArrayVec::new();
        v.push(1).unwrap();
        v.push(2).unwrap();
        assert_eq!(v.pop_front(), Some(1));
        assert_eq!(v.pop_front(), Some(2));
        assert_eq!(v.pop_front(), None);
    }
}
