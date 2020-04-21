//! In the flat_tree structure, every "node" is just a line, there's
//!  no link from a child to its parent or from a parent to its children.
use {
    crate::{
        errors,
        file_sizes::FileSize,
        git::{
            LineGitStatus,
            TreeGitStatus,
        },
        task_sync::{
            ComputationResult,
        },
        selection_type::SelectionType,
        task_sync::Dam,
        tree_build::TreeBuilder,
        tree_options::TreeOptions,
    },
    std::{
        cmp::{self, Ord, Ordering, PartialOrd},
        fs,
        mem,
        path::{Path, PathBuf},
        time::SystemTime,
    },
};

#[cfg(unix)]
use {
    std::os::unix::fs::MetadataExt,
    umask::Mode,
};

#[cfg(windows)]
use is_executable::IsExecutable;

#[derive(Debug, Clone, PartialEq)]
pub enum LineType {
    File,
    Dir,
    SymLinkToDir(String),
    SymLinkToFile(String), // (to file or to symlink)
    Pruning,               // a "xxx unlisted" line
}

/// a line in the representation of the file hierarchy
#[derive(Debug, Clone)]
pub struct TreeLine {
    pub left_branchs: Box<[bool]>, // a depth-sized array telling whether a branch pass
    pub depth: u16,
    pub name: String,
    pub path: PathBuf,
    pub line_type: LineType,
    pub has_error: bool,
    pub nb_kept_children: usize,
    pub unlisted: usize, // number of not listed children (Dir) or brothers (Pruning)
    pub score: i32,      // 0 if there's no pattern
    pub size: Option<FileSize>, // None when not measured
    pub metadata: fs::Metadata,
    pub git_status: Option<LineGitStatus>,
}

#[derive(Debug, Clone)]
pub struct Tree {
    pub lines: Box<[TreeLine]>,
    pub selection: usize, // there's always a selection (starts with root, which is 0)
    pub options: TreeOptions,
    pub scroll: i32, // the number of lines at the top hidden because of scrolling
    pub nb_gitignored: u32, // number of times a gitignore pattern excluded a file
    pub total_search: bool, // whether the search was made on all children
    pub git_status: ComputationResult<TreeGitStatus>,
}

impl TreeLine {
    pub fn is_selectable(&self) -> bool {
        match &self.line_type {
            LineType::Pruning => false,
            _ => true,
        }
    }
    pub fn is_dir(&self) -> bool {
        match &self.line_type {
            LineType::Dir => true,
            LineType::SymLinkToDir(_) => true,
            _ => false,
        }
    }
    pub fn is_file(&self) -> bool {
        match &self.line_type {
            LineType::File => true,
            _ => false,
        }
    }
    pub fn is_of(&self, selection_type: SelectionType) -> bool {
        match selection_type {
            SelectionType::Any => true,
            SelectionType::File => self.is_file(),
            SelectionType::Directory => self.is_dir(),
        }
    }
    pub fn selection_type(&self) -> SelectionType {
        use LineType::*;
        match &self.line_type {
            File | SymLinkToFile(_) => SelectionType::File,
            Dir | SymLinkToDir(_) => SelectionType::Directory,
            Pruning => SelectionType::Any, // should not happen today
        }
    }
    #[cfg(unix)]
    pub fn mode(&self) -> Mode {
        Mode::from(self.metadata.mode())
    }
    pub fn is_exe(&self) -> bool {
        #[cfg(unix)]
        return self.mode().is_exe();

        #[cfg(windows)]
        return self.path.is_executable();
    }
    /// build and return the absolute targeted path: either self.path or the
    ///  solved canonicalized symlink
    /// (the path may be invalid if the symlink is)
    pub fn target(&self) -> PathBuf {
        match &self.line_type {
            LineType::SymLinkToFile(target) | LineType::SymLinkToDir(target) => {
                let mut target_path = PathBuf::from(target);
                if target_path.is_relative() {
                    target_path = self.path.parent().unwrap().join(target_path);
                }
                if let Ok(canonic) = fs::canonicalize(&target_path) {
                    target_path = canonic;
                }
                target_path
            }
            _ => self.path.clone(),
        }
    }
    /// return the last modification date, if it makes sense
    pub fn modified(&self) -> Option<SystemTime> {
        match &self.line_type {
            LineType::Pruning => None,
            _ => self.metadata.modified().ok(),
        }
    }
}
impl PartialEq for TreeLine {
    fn eq(&self, other: &TreeLine) -> bool {
        self.path == other.path
    }
}

impl Eq for TreeLine {}

impl Ord for TreeLine {
    // paths are sorted in a complete ignore case way
    // (A<a<B<b)
    fn cmp(&self, other: &TreeLine) -> Ordering {
        let mut sci = self.path.components();
        let mut oci = other.path.components();
        loop {
            match sci.next() {
                Some(sc) => {
                    match oci.next() {
                        Some(oc) => {
                            let scs = sc.as_os_str().to_string_lossy();
                            let ocs = oc.as_os_str().to_string_lossy();
                            let lower_ordering = scs.to_lowercase().cmp(&ocs.to_lowercase());
                            if lower_ordering != Ordering::Equal {
                                return lower_ordering;
                            }
                            let ordering = scs.cmp(&ocs);
                            if ordering != Ordering::Equal {
                                return ordering;
                            }
                        }
                        None => {
                            return Ordering::Greater;
                        }
                    };
                }
                None => {
                    if oci.next().is_some() {
                        return Ordering::Less;
                    } else {
                        return Ordering::Equal;
                    }
                }
            };
        }
    }
}

impl PartialOrd for TreeLine {
    fn partial_cmp(&self, other: &TreeLine) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Tree {
    pub fn refresh(&mut self, page_height: usize) -> Result<(), errors::TreeBuildError> {
        let builder = TreeBuilder::from(
            	self.root().to_path_buf(),
            	self.options.clone(),
            	page_height,
            )?;
        let mut tree = builder.build(
            	false, // on refresh we always do a non total search
                &Dam::unlimited(),
            ).unwrap(); // should not fail
        // we save the old selection to try restore it
        let selected_path = self.selected_line().path.to_path_buf();
        mem::swap(&mut self.lines, &mut tree.lines);
        self.scroll = 0;
        if !self.try_select_path(&selected_path) {
            if self.selection >= self.lines.len() {
                self.selection = 0;
            }
        }
        self.make_selection_visible(page_height as i32);
        Ok(())
    }


    /// do what must be done after line additions or removals:
    /// - sort the lines
    /// - compute left branchs
    pub fn after_lines_changed(&mut self) {
        // we sort the lines (this is mandatory to avoid crashes)
        self.lines[1..].sort();

        let mut best_index = 0; // index of the line with the best score
        for i in 1..self.lines.len() {
            if self.lines[i].score > self.lines[best_index].score {
                best_index = i;
            }
            for d in 0..self.lines[i].left_branchs.len() {
                self.lines[i].left_branchs[d] = false;
            }
        }
        // then we discover the branches (for the drawing)
        // and we mark the last children as pruning, if they have unlisted brothers
        let mut last_parent_index: usize = self.lines.len() + 1;
        for end_index in (1..self.lines.len()).rev() {
            let depth = (self.lines[end_index].depth - 1) as usize;
            let start_index = {
                let parent_index = {
                    let parent_path = &self.lines[end_index].path.parent();
                    match parent_path {
                        Some(parent_path) => {
                            let mut index = end_index;
                            loop {
                                index -= 1;
                                if self.lines[index].path == *parent_path {
                                    break;
                                }
                                if index == 0 {
                                    break;
                                }
                            }
                            index
                        }
                        None => end_index, // Should not happen
                    }
                };
                if parent_index != last_parent_index {
                    // the line at end_index is the last listed child of the line at parent_index
                    let unlisted = self.lines[parent_index].unlisted;
                    if unlisted > 0 && self.lines[end_index].nb_kept_children == 0 {
                        if best_index == end_index {
                            debug!("Avoiding to prune the line with best score");
                        } else {
                            debug!("turning {:?} into Pruning", self.lines[end_index].path);
                            self.lines[end_index].line_type = LineType::Pruning;
                            self.lines[end_index].unlisted = unlisted + 1;
                            self.lines[end_index].name = format!("{} unlisted", unlisted + 1);
                            self.lines[parent_index].unlisted = 0;
                        }
                    }
                    last_parent_index = parent_index;
                }
                parent_index + 1
            };
            for i in start_index..=end_index {
                self.lines[i].left_branchs[depth] = true;
            }
        }
    }
    pub fn has_branch(&self, line_index: usize, depth: usize) -> bool {
        if line_index >= self.lines.len() {
            return false;
        }
        let line = &self.lines[line_index];
        depth < usize::from(line.depth) && line.left_branchs[depth]
    }
    /// select another line
    ///
    /// For example the following one if dy is 1.
    pub fn move_selection(&mut self, dy: i32, page_height: i32) {
        // only work for +1 or -1
        let l = self.lines.len();
        loop {
            self.selection = (self.selection + ((l as i32) + dy) as usize) % l;
            if self.lines[self.selection].is_selectable() {
                break;
            }
        }
        // we adjust the scroll
        let l = l as i32;
        let sel = self.selection as i32;
        if l > page_height {
            if dy < 0 { // -1
                if sel == l - 1 { // cycling
                    self.scroll = l - page_height;
                } else if sel < self.scroll + 5 {
                    self.scroll = (self.scroll + 2 * dy).max(0);
                }
            } else { // +1
                if sel == 0 { // cycling brought us back to top
                    self.scroll = 0;
                } else if sel > self.scroll + page_height - 5 {
                    self.scroll = (self.scroll + 2 * dy).min(l - page_height);
                }
            }
        }
    }
    pub fn try_scroll(&mut self, dy: i32, page_height: i32) {
        self.scroll = (self.scroll + dy).max(0).min(self.lines.len() as i32 - 5);
        self.select_visible_line(page_height);
    }
    /// try to select a line (works if y+scroll falls on a selectable line)
    pub fn try_select_y(&mut self, y: i32) -> bool {
        let y = y + self.scroll;
        if y >= 0 && y < self.lines.len() as i32 {
            let y = y as usize;
            if self.lines[y].is_selectable() {
                self.selection = y;
                return true;
            }
        }
        false
    }
    /// fix the selection so that it's a selectable visible line
    fn select_visible_line(&mut self, page_height: i32) {
        let sel = self.selection as i32;
        if sel < self.scroll || sel >= self.scroll + page_height {
            self.selection = self.scroll as usize;
            let l = self.lines.len();
            loop {
                self.selection = (self.selection + ((l as i32) + 1) as usize) % l;
                if self.lines[self.selection].is_selectable() {
                    break;
                }
            }
        }
    }
    pub fn make_selection_visible(&mut self, page_height: i32) {
        let sel = self.selection as i32;
        let l = self.lines.len() as i32;
        if sel < self.scroll {
            self.scroll = (self.selection as i32 - 2).max(0);
        } else if l > page_height && sel >= self.scroll + page_height {
            self.scroll = (self.selection as i32 - page_height + 2) as i32;
        }
    }
    pub fn selected_line(&self) -> &TreeLine {
        &self.lines[self.selection]
    }
    pub fn root(&self) -> &PathBuf {
        &self.lines[0].path
    }
    // select the line with the best matching score
    pub fn try_select_best_match(&mut self) {
        let mut best_score = 0;
        for (idx, line) in self.lines.iter().enumerate() {
            if !line.is_selectable() {
                continue;
            }
            if best_score > line.score {
                continue;
            }
            if line.score == best_score {
                // in case of equal scores, we prefer the shortest path
                if self.lines[idx].depth >= self.lines[self.selection].depth {
                    continue;
                }
            }
            best_score = line.score;
            self.selection = idx;
        }
    }
    /// return true when we could select the given path
    pub fn try_select_path(&mut self, path: &Path) -> bool {
        for (idx, line) in self.lines.iter().enumerate() {
            if !line.is_selectable() {
                continue;
            }
            if path == line.path {
                self.selection = idx;
                return true;
            }
        }
        false
    }
    pub fn try_select_first(&mut self) -> bool {
        for idx in 0..self.lines.len() {
            let line = &self.lines[idx];
            if line.is_selectable() {
                self.selection = idx;
                return true;
            }
        }
        false
    }
    pub fn try_select_last(&mut self) -> bool {
        for idx in (0..self.lines.len()).rev() {
            let line = &self.lines[idx];
            if line.is_selectable() {
                self.selection = idx;
                return true;
            }
        }
        false
    }
    pub fn try_select_next_match(&mut self) -> bool {
        for di in 0..self.lines.len() {
            let idx = (self.selection + di + 1) % self.lines.len();
            let line = &self.lines[idx];
            if !line.is_selectable() {
                continue;
            }
            // Not very elegant
            // If we implement several types of search (path, content),
            // we'll rather store a direct_match boolean in the line.
            if self.options.pattern.is_some()
                && self.options.pattern.score_of(&line.name).is_none() {
                continue;
            }
            if line.score > 0 {
                self.selection = idx;
                return true;
            }
        }
        false
    }
    pub fn try_select_previous_match(&mut self) -> bool {
        for di in (0..self.lines.len()).rev() {
            let idx = (self.selection + di) % self.lines.len();
            let line = &self.lines[idx];
            if !line.is_selectable() {
                continue;
            }
            if line.score > 0 {
                self.selection = idx;
                return true;
            }
        }
        false
    }

    pub fn has_dir_missing_size(&self) -> bool {
        self.options.show_sizes
            && self.lines.iter().skip(1).any(|line|
                line.line_type == LineType::Dir && line.size.is_none()
            )
    }

    pub fn is_missing_git_status_computation(&self) -> bool {
        self.git_status.is_not_computed()
    }

    pub fn fetch_file_sizes(&mut self) {
        for i in 1..self.lines.len() {
            if self.lines[i].is_file() {
                self.lines[i].size = Some(FileSize::from_file(&self.lines[i].path));
            }
        }
        self.sort_siblings_by_size();
    }

    /// compute the size of one directory
    ///
    /// To compute the size of all of them, this should be called until
    ///  has_dir_missing_size returns false
    pub fn fetch_some_missing_dir_size(&mut self, dam: &Dam) {
        for i in 1..self.lines.len() {
            if self.lines[i].size.is_none() && self.lines[i].line_type == LineType::Dir {
                self.lines[i].size = FileSize::from_dir(&self.lines[i].path, dam);
                self.sort_siblings_by_size();
                return;
            }
        }
    }
    /// Sort files according to their size
    ///
    /// Warning: must not be called if there's more than one level displayed!
    /// (a better sort should be devised but it's unsure whether it would be
    /// readable enough)
    fn sort_siblings_by_size(&mut self) {
        // we'll try to keep the same path selected
        let selected_path = self.selected_line().path.to_path_buf();
        self.lines[1..].sort_by(|a, b| {
            let asize = a.size.map_or(0, |s| s.into());
            let bsize = b.size.map_or(0, |s| s.into());
            bsize.cmp(&asize)
        });
        self.try_select_path(&selected_path);
    }

    /// compute and return the size of the root
    pub fn total_size(&self) -> FileSize {
        if let Some(size) = self.lines[0].size {
            // if the real total size is computed, it's in the root line
            size
        } else {
            // if we don't have the size in root, the nearest estimate is
            // the sum of sizes of lines at depth 1
            let mut sum = FileSize::new(0, false);
            for i in 1..self.lines.len() {
                if self.lines[i].depth == 1 {
                    if let Some(size) = self.lines[i].size {
                        sum += size;
                    }
                }
            }
            sum
        }
    }
}
