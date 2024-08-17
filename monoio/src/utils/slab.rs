//! Part of code and design forked from tokio.

use std::{mem, mem::MaybeUninit, ops::{Deref, DerefMut}};

const NUM_PAGES: usize = 26;
const PAGE_INITIAL_SIZE: usize = 64;
const COMPACT_INTERVAL: u32 = 2048;

/// Pre-allocated storage for a uniform data type
#[derive(Default)]
pub(crate) struct Slab<T> {
    // pages of continued memory
    pages: [Option<Page<T>>; NUM_PAGES],

    currentWritePageIndex: usize,

    // current generation
    generation: u32,
}

impl<T> Slab<T> {
    pub(crate) const fn new() -> Slab<T> {
        Slab {
            pages: [
                None, None, None, None, None, None, None, None, None, None, None, None, None, None,
                None, None, None, None, None, None, None, None, None, None, None, None,
            ],
            currentWritePageIndex: 0,
            generation: 0,
        }
    }

    #[allow(unused)]
    pub(crate) fn len(&self) -> usize {
        self.pages.iter().fold(0, |acc, page| match page {
            Some(page) => acc + page.usedEntryCount,
            None => acc,
        })
    }

    pub(crate) fn get(&mut self, key: usize) -> Option<Ref<'_, T>> {
        let pageIndex = getPageIndex(key);

        // here we make 2 mut ref so we must make it safe.
        let slab = unsafe { &mut *(self as *mut Slab<T>) };

        let page = match unsafe { self.pages.get_unchecked_mut(pageIndex) } {
            Some(page) => page,
            None => return None,
        };

        let entryIndex = key - page.entryCountInPrevPages;

        match page.get_entry_mut(entryIndex) {
            None => None,
            Some(entry) => match entry {
                Entry::Vacant(_) => None,
                Entry::Occupied(_) => Some(Ref { slab, page, entryIndex }),
            },
        }
    }

    /// Insert an element into slab. The key is returned.
    /// Note: If the slab is out of slot, it will panic.
    pub(crate) fn insert(&mut self, val: T) -> usize {
        let currentWritePageIndex = self.currentWritePageIndex;

        for writePageIndex in currentWritePageIndex..NUM_PAGES {
            unsafe {
                let page = match self.pages.get_unchecked_mut(writePageIndex) {
                    Some(page) => page,
                    None => {
                        let page = Page::new(PAGE_INITIAL_SIZE << writePageIndex, (PAGE_INITIAL_SIZE << writePageIndex) - PAGE_INITIAL_SIZE);
                        let r = self.pages.get_unchecked_mut(writePageIndex);
                        *r = Some(page);
                        r.as_mut().unwrap_unchecked()
                    }
                };

                if let Some(entryIndex) = page.alloc() {
                    page.set(entryIndex, val);
                    self.currentWritePageIndex = writePageIndex;
                    return entryIndex + page.entryCountInPrevPages;
                }
            }
        }

        panic!("out of slot");
    }

    #[allow(unused)]
    pub(crate) fn remove(&mut self, key: usize) -> Option<T> {
        let page_id = getPageIndex(key);
        let page = match unsafe { self.pages.get_unchecked_mut(page_id) } {
            Some(page) => page,
            None => return None,
        };
        let val = page.remove(key - page.entryCountInPrevPages);
        self.mark_remove();
        val
    }

    pub(crate) fn mark_remove(&mut self) {
        // compact
        self.generation = self.generation.wrapping_add(1);

        if self.generation % COMPACT_INTERVAL == 0 {
            // reset write page index
            self.currentWritePageIndex = 0;

            // find the last allocated page and try to drop
            if let Some((pageIndex, last_page)) = self.pages.iter_mut().enumerate().rev().find_map(|(pageIndex, page)| page.as_mut().map(|page| (pageIndex, page))) {
                if last_page.is_empty() && pageIndex > 0 {
                    unsafe {
                        *self.pages.get_unchecked_mut(pageIndex) = None;
                    }
                }
            }
        }
    }
}

// Forked from tokio.
fn getPageIndex(key: usize) -> usize {
    const POINTER_WIDTH: u32 = size_of::<usize>() as u32 * 8;
    const PAGE_INDEX_SHIFT: u32 = PAGE_INITIAL_SIZE.trailing_zeros() + 1;

    let slot_shifted = (key.saturating_add(PAGE_INITIAL_SIZE)) >> PAGE_INDEX_SHIFT;
    ((POINTER_WIDTH - slot_shifted.leading_zeros()) as usize).min(NUM_PAGES - 1)
}

/// Ref point to a valid slot.
pub(crate) struct Ref<'a, T> {
    slab: &'a mut Slab<T>,
    page: &'a mut Page<T>,
    entryIndex: usize,
}

impl<'a, T> Ref<'a, T> {
    #[allow(unused)]
    pub(crate) fn remove(self) -> T {
        let val = unsafe { self.page.remove(self.entryIndex).unwrap_unchecked() };
        self.slab.mark_remove();
        val
    }
}

impl<'a, T> AsRef<T> for Ref<'a, T> {
    fn as_ref(&self) -> &T {
        unsafe { self.page.get(self.entryIndex).unwrap_unchecked() }
    }
}

impl<'a, T> AsMut<T> for Ref<'a, T> {
    fn as_mut(&mut self) -> &mut T {
        unsafe { self.page.get_mut(self.entryIndex).unwrap_unchecked() }
    }
}

impl<'a, T> Deref for Ref<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl<'a, T> DerefMut for Ref<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut()
    }
}

struct Page<T> {
    // continued buffer of fixed size
    entries: Box<[MaybeUninit<Entry<T>>]>,
    usedEntryCount: usize,
    /// 只会增加
    initializedEntryCount: usize,
    nextWritableEntryIndex: usize,
    entryCountInPrevPages: usize,
}

impl<T> Page<T> {
    fn new(size: usize, prev_len: usize) -> Self {
        let mut buffer = Vec::with_capacity(size);
        unsafe { buffer.set_len(size) };

        let slots = buffer.into_boxed_slice();

        Self {
            entries: slots,
            usedEntryCount: 0,
            initializedEntryCount: 0,
            nextWritableEntryIndex: 0,
            entryCountInPrevPages: prev_len,
        }
    }

    fn is_empty(&self) -> bool {
        self.usedEntryCount == 0
    }

    fn is_full(&self) -> bool {
        self.usedEntryCount == self.entries.len()
    }

    // after allocated, the caller must guarantee it will be initialized
    unsafe fn alloc(&mut self) -> Option<usize> {
        let nextWritableEntryIndex = self.nextWritableEntryIndex;

        if self.is_full() {
            debug_assert_eq!(nextWritableEntryIndex, self.entries.len(), "next should eq to slots.len()");
            return None;
        }

        // the slot to write is not initialized
        if nextWritableEntryIndex >= self.initializedEntryCount {
            debug_assert_eq!(nextWritableEntryIndex, self.initializedEntryCount, "next should eq to initialized");
            self.initializedEntryCount += 1;
            self.nextWritableEntryIndex += 1;
        } else {
            // the slot has already been initialized, it must be Vacant
            let slot = self.entries.get_unchecked(nextWritableEntryIndex).assume_init_ref();
            match slot {
                Entry::Vacant(nextWritableEntryIndex) =>  self.nextWritableEntryIndex = *nextWritableEntryIndex,
                _ => std::hint::unreachable_unchecked(),
            }
        }

        self.usedEntryCount += 1;

        Some(nextWritableEntryIndex)
    }

    // Safety: the entryIndex must be by Self::alloc.
    unsafe fn set(&mut self, entryIndex: usize, val: T) {
        let entry = self.entries.get_unchecked_mut(entryIndex);
        *entry = MaybeUninit::new(Entry::Occupied(val));
    }

    fn get(&self, slot: usize) -> Option<&T> {
        if slot >= self.initializedEntryCount {
            return None;
        }

        unsafe { self.entries.get_unchecked(slot).assume_init_ref() }.as_ref()
    }

    fn get_mut(&mut self, slot: usize) -> Option<&mut T> {
        if slot >= self.initializedEntryCount {
            return None;
        }

        unsafe { self.entries.get_unchecked_mut(slot).assume_init_mut() }.as_mut()
    }

    fn get_entry_mut(&mut self, slot: usize) -> Option<&mut Entry<T>> {
        if slot >= self.initializedEntryCount {
            return None;
        }

        unsafe { Some(self.entries.get_unchecked_mut(slot).assume_init_mut()) }
    }

    fn remove(&mut self, entryIndex: usize) -> Option<T> {
        if entryIndex >= self.initializedEntryCount {
            return None;
        }

        unsafe {
            let entry = self.entries.get_unchecked_mut(entryIndex).assume_init_mut();
            if entry.is_vacant() {
                return None;
            }

            let entry = mem::replace(entry, Entry::Vacant(self.nextWritableEntryIndex));
            self.nextWritableEntryIndex = entryIndex;
            self.usedEntryCount -= 1;

            Some(entry.unwrap_unchecked())
        }
    }
}

impl<T> Drop for Page<T> {
    fn drop(&mut self) {
        let mut to_drop = mem::take(&mut self.entries).into_vec();

        unsafe {
            if self.is_empty() {
                // fast drop if empty
                to_drop.set_len(0);
            } else {
                // slow drop
                to_drop.set_len(self.initializedEntryCount);
                mem::transmute::<_, Vec<Entry<T>>>(to_drop);
            }
        }
    }
}

enum Entry<T> {
    /// next entry index in page
    Vacant(usize),
    Occupied(T),
}

impl<T> Entry<T> {
    fn as_ref(&self) -> Option<&T> {
        match self {
            Entry::Vacant(_) => None,
            Entry::Occupied(inner) => Some(inner),
        }
    }

    fn as_mut(&mut self) -> Option<&mut T> {
        match self {
            Entry::Vacant(_) => None,
            Entry::Occupied(inner) => Some(inner),
        }
    }

    fn is_vacant(&self) -> bool {
        matches!(self, Entry::Vacant(_))
    }

    unsafe fn unwrap_unchecked(self) -> T {
        match self {
            Entry::Vacant(_) => std::hint::unreachable_unchecked(),
            Entry::Occupied(inner) => inner,
        }
    }
}
