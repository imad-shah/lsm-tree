use crate::entry::Entry;
use std::iter::Peekable;

pub(crate) type Source<'a> = Box<dyn Iterator<Item = (&'a [u8], &'a Entry)> + 'a>;

pub(crate) struct MergeIterator<'a> {
    sources: Vec<Peekable<Source<'a>>>,
}

impl<'a> MergeIterator<'a> {
    pub(crate) fn new(sources: Vec<Source<'a>>) -> Self {
        Self {
            sources: sources.into_iter().map(|s| s.peekable()).collect(),
        }
    }
}

impl<'a> Iterator for MergeIterator<'a> {
    type Item = (&'a [u8], &'a Entry);

    fn next(&mut self) -> Option<Self::Item> {
        // pass one: find the smallest item, earlier source wins ties
        let mut winner: Option<Self::Item> = None;
        for s in self.sources.iter_mut() {
            if let Some(contender) = s.peek().copied() {
                match winner {
                    None => winner = Some(contender),
                    Some(w) if contender.0 < w.0 => winner = Some(contender),
                    _ => {}
                }
            }
        }
        // if winner is None, end early
        let winner = winner?;

        // pass two: move every iterator that equals winner forward
        for s in self.sources.iter_mut() {
            if let Some(&(key, _)) = s.peek()
                && key == winner.0
            {
                s.next();
            }
        }
        Some(winner)
    }
}
