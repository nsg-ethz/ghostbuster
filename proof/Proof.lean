import Mathlib
set_option linter.style.header false
set_option linter.unusedSectionVars false
set_option linter.unusedDecidableInType false
set_option linter.unusedFintypeInType false
set_option linter.unusedVariables false

namespace GhostBuster

-- Define variables for Neighbors and Routes that are arbitrarily.
variable {Neighbor Route : Type*}
  [Fintype Neighbor] [DecidableEq Neighbor]
  [DecidableEq Route]

local notation:max "Msg" => Option (Route × ℝ)
local notation:max "State" => Neighbor -> Msg
local notation:max "Seq" => Neighbor -> List Msg

-- We assume the spec as an axiom
axiom spec : State -> Msg

-- whether a message matches an output, the routes must match, and the output must be message must
-- be before the output.
def InpMatchesOut (a b : Msg) : Prop :=
  match a, b with
  | none, none => True
  | some (ma, ta), some (mb, tb) => ma = mb ∧ ta <= tb
  | _, _ => False

infix:50 " ≊ " => InpMatchesOut

-- fork : Seq
-- n : Neighbor
-- m : Msg
-- uncommitted : Seq
-- ∀ prefix, prefix <+: uncommitted n → router (fun n' => if n' == n fork n ++ prefix else []) == m


def SeqSub (a b : Seq) : Prop := ∀ n, a n <+: b n
def state (seq : Seq) : State := fun n => (seq n).getLast?.join
def empty : Seq := fun _ => []
def seqLength (seq : Seq) : ℕ := ∑ n, (seq n).length

infix:50 " ⊑ " => SeqSub

-- arguments: prev witness out
inductive WitnessFrom :
    Seq -> List Seq -> List Msg -> Prop where
  | nil (prev : Seq) : WitnessFrom prev [] []
  | cons {prev w ws o os} :
      prev ⊑ w → spec (state w) ≊ o →
      WitnessFrom w ws os →
      WitnessFrom prev (w :: ws) (o :: os)

-- There exists a witness that reaches the end of the output.
-- This property does not check if the input is fully processed.
def WitnessReaches (prev i : Seq) (o : List Msg) : Prop :=
  ∃ w, WitnessFrom prev w o ∧ (w.getLast?.getD prev) ⊑ i

def ValidFrom (prev i : Seq) (o : List Msg) : Prop :=
  WitnessReaches prev i o ∧ spec (state i) ≊ o.getLast?.join

def MinStep (prev m : Seq) (o : Msg) (ref : State -> Msg) : Prop :=
  prev ⊑ m ∧ ref (state m) ≊ o ∧
  ∀ inbetween, prev ⊑ inbetween →
    inbetween ⊑ m →
    spec ( state inbetween) ≊ o → m = inbetween

inductive MinWitnessFrom :
    List Msg -> Seq -> List Seq -> Prop where
  | nil (prev : Seq) : MinWitnessFrom [] prev []
  | cons {o os prev w ws}  :
      MinStep prev w o spec ->
      MinWitnessFrom os w ws ->
      MinWitnessFrom (o :: os) prev (w :: ws)

-- A pseudocode algorithm that produces a witness step-by-step

-- a single step
def GhostBusterStep (i : Seq) (prev : Seq) (o : Msg) (ref : State -> Msg) : Set Seq :=
  { m : Seq | m ⊑ i ∧ MinStep prev m o ref }

-- a recursive lookup without caring about the final state matching the last out message.
def GhostBusterRun (i : Seq) (prev : Seq) (o : List Msg) (ref : State -> Msg) : Set (List Seq) :=
  match o with
  | [] => {w | w = [] ∧ prev ⊑ i}
  | o :: os => { m :: ws | (m ∈ GhostBusterStep i prev o ref) (ws ∈ GhostBusterRun i m os ref) }

-- The full algorithm that also checks if the last state in i matches o.
def GhostBusterAlg (i : Seq) (prev : Seq) (o : List Msg) (ref : State -> Msg) : Set (List Seq) :=
  { w ∈ GhostBusterRun i prev o ref | ref (state i) ≊ o.getLast?.join }

lemma sub_refl (seq : Seq) : seq ⊑ seq := fun n => List.prefix_refl _
lemma sub_trans {a b c : Seq} (hab : a ⊑ b) (hbc : b ⊑ c) : a ⊑ c := fun n => (hab n).trans (hbc n)
lemma seq_length_mono {a b : Seq} (hab : a ⊑ b) : seqLength a ≤ seqLength b :=
  Finset.sum_le_sum (fun n _ => (hab n).length_le)

/-- A subsequence with the same total length is in fact equal. -/
lemma eq_of_sub_eq_length {a b : Seq}
    (hsub : a ⊑ b) (h_size : seqLength a = seqLength b) : a = b := by
  funext n
  -- derive that the length is equal for each element
  have hlen : ∀ m ∈ Finset.univ, (a m).length = (b m).length :=
    (Finset.sum_eq_sum_iff_of_le (fun m _ => (hsub m).length_le)).mp h_size
  exact (hsub n).eq_of_length (hlen n (Finset.mem_univ n))

/-- Within the interval `[a, b]` there is a minimal sequence producing `target`. -/
lemma min_sequence_exists
    (a b : Seq) (out : Msg)
    (hab : a ⊑ b) (hbspec : spec (state b) ≊ out) :
    ∃ minimal, MinStep a minimal out spec ∧ minimal ⊑ b := by
  have hwf : WellFounded (fun a b : Seq => seqLength a < seqLength b) :=
    InvImage.wf seqLength Nat.lt_wfRel.wf
  obtain ⟨m, ⟨ham, hmb, hmspec⟩, hmin⟩ := hwf.has_min
    {x | a ⊑ x ∧ x ⊑ b ∧ spec (state x) ≊ out}
    ⟨b, hab, sub_refl b, hbspec⟩
  refine ⟨m, ⟨ham, hmspec, ?_⟩, hmb⟩
  intro x hax hxm hxspec
  have h_not_xltm : ¬ seqLength x < seqLength m := hmin x ⟨ hax, sub_trans hxm hmb, hxspec ⟩
  exact
    (eq_of_sub_eq_length hxm (le_antisymm (seq_length_mono hxm) (Nat.le_of_not_lt h_not_xltm))).symm

/-- Lowering the lower bound preserves `IsWitnessFrom`. -/
lemma witness_from_mono {out : List Msg} {a b : Seq} {w : List Seq}
    (hab : a ⊑ b) (hw : WitnessFrom b w out) : WitnessFrom a w out := by
  cases hw with
  | nil =>
    exact .nil _
  | cons hbw hwspec htail =>
    exact .cons (sub_trans hab hbw) hwspec htail

/-- The last elements of elementwise-related witnesses are related. -/
lemma forall_pair_sub_last_sub {w₁ w₂ : List Seq} (default : Seq)
    (h : List.Forall₂ (· ⊑ ·) w₁ w₂) :
    (w₁.getLast?.getD default) ⊑ (w₂.getLast?.getD default) := by
  induction h with
  | nil => exact sub_refl _
  | cons hab htail ih =>
      cases htail with
      | nil => simpa using hab
      | cons _ _ => rw [List.getLast?_cons_cons, List.getLast?_cons_cons]; exact ih

/-- Core: any witness can be made minimal, staying elementwise below the original. -/
lemma minimal_witness_from_exists :
    ∀ (o : List Msg) (prev : Seq) (w : List Seq),
      WitnessFrom prev w o →
      ∃ m, WitnessFrom prev m o ∧ MinWitnessFrom o prev m ∧
            List.Forall₂ (· ⊑ ·) m w := by
  intro out
  induction out with
  | nil =>
      intro prev w hw
      -- as out is [] and hw: IsWitnessFrom spec [] prev w, it must be that w is also []. The proof
      -- of this is essentially the inductive definition of hw.
      cases hw with
      | nil =>
        exact ⟨[], .nil _, .nil _, .nil⟩
  | cons o os ih =>
      intro prev w hw
      cases hw with
      | cons hpw hspec htail =>
          -- there must exist a minimal sequence `x` that still satisfies the spec
          obtain ⟨x, hxmin, hxw⟩ := min_sequence_exists prev _ o hpw hspec
          -- that minimal sequence does not remove the witness property from w.
          obtain ⟨mw, hmw, hmmin, hforall⟩ := ih x _ (witness_from_mono hxw htail)
          -- Now, we construct a new minimal witness that concatenates the minimal sequence with
          -- the minimal witness of the remainder of the list.
          exact ⟨x :: mw, .cons hxmin.1 hxmin.2.1 hmw, .cons hxmin hmmin, .cons hxw hforall⟩

theorem valid_minimal_witness_exists :
    ∀ (prev i : Seq) (o : List Msg),
      ValidFrom prev i o ↔ ∃ w, WitnessFrom prev w o ∧
                                 MinWitnessFrom o prev w ∧
                                 (w.getLast?.getD prev) ⊑ i ∧
                                 spec (state i) ≊ o.getLast?.join := by
  intro prev i o
  constructor
  · rw [ValidFrom]
    rintro ⟨⟨w, hwf, hprefix⟩, hispec⟩
    obtain ⟨ m, hwf, hmwf, hforall ⟩ := minimal_witness_from_exists o prev w hwf
    exact ⟨ m, hwf, hmwf, sub_trans (forall_pair_sub_last_sub prev hforall) hprefix, hispec⟩
  · rintro ⟨ w, hwf, hmwf, hlast, hispec ⟩
    exact ⟨⟨w, hwf, hlast⟩, hispec⟩


-- lemmas about getLast? on non-empty lists l
lemma getLast?_cons_ne {l : List Seq} (hl : l ≠ []) (v : Seq) :
    (v :: l).getLast? = l.getLast? := by
  obtain ⟨x, xs, rfl⟩ := List.exists_cons_of_ne_nil hl
  rfl

lemma getlast?_cons_getD_indep {l : List Seq} (hl : l ≠ []) (a b : Seq) :
    l.getLast?.getD a = l.getLast?.getD b := by
  obtain ⟨x, xs, rfl⟩ := List.exists_cons_of_ne_nil hl
  induction xs generalizing x with
  | nil => rfl
  | cons y ys ih =>
    rw [getLast?_cons_ne (by simp) x]
    exact ih y (by simp)

lemma getLast?_cons_getD {l : List Seq} (hl : l ≠ []) (v a b : Seq) :
    (v :: l).getLast?.getD a = l.getLast?.getD b := by
  rw [getLast?_cons_ne hl]
  exact getlast?_cons_getD_indep hl a b

lemma getLast?_cons_getD_eq (prev w : Seq) (ws : List Seq) :
    (w :: ws).getLast?.getD prev = ws.getLast?.getD w := by
  cases ws with
  | nil => rfl
  | cons w' ws' =>
    rw [getLast?_cons_ne (by simp)]
    exact getlast?_cons_getD_indep (by simp) prev w

-- the lower bound sits below the last witness state
lemma witness_from_sub_last {prev : Seq} {w : List Seq} {out : List Msg}
    (h : WitnessFrom prev w out) : prev ⊑ w.getLast?.getD prev := by
  induction h with
  | nil => exact sub_refl _
  | @cons p w ws o os hpw hwspec htail ih =>
    cases ws with
    | nil => simpa using hpw
    | cons w' ws' =>
      rw [getLast?_cons_getD (by simp)]
      exact sub_trans hpw ih

-- a partial witness run keeps you below the input
lemma witness_reaches {prev i : Seq} {os : List Msg}
    (h : WitnessReaches prev i os) : prev ⊑ i := by
  obtain ⟨ws, hwf, hlast⟩ := h
  exact sub_trans (witness_from_sub_last hwf) hlast

-- If something is valid for a prev, you can make the prev smaller while keeping the validity.
lemma witness_reaches_mono {a b i : Seq} {os : List Msg}
    (hab : a ⊑ b) (h : WitnessReaches b i os) : WitnessReaches a i os := by
  obtain ⟨ws, hwf, hlast⟩ := h
  refine ⟨ws, witness_from_mono hab hwf, ?_⟩
  cases ws with
  | nil => exact sub_trans hab hlast
  | cons w ws => rw [getlast?_cons_getD_indep (by simp) a b]; exact hlast

-- witness_reaches can be build up step-by-step.
lemma witness_reaches_cons {prev i : Seq} {o : Msg} {os : List Msg} :
    WitnessReaches prev i (o :: os) ↔
      ∃ w, prev ⊑ w ∧ spec (state w) ≊ o ∧ WitnessReaches w i os := by
  constructor
  · rintro ⟨W, hwf, hlast⟩
    cases hwf with
    | cons hpw hwspec htail =>
      rename_i w ws -- get out the w and ws from ValidFrom
      refine ⟨w, hpw, hwspec, ws, htail, ?_⟩
      rw [getLast?_cons_getD_eq] at hlast
      exact hlast
  · rintro ⟨w, hpw, hwspec, ws, hwf, hlast⟩
    refine ⟨w :: ws, .cons hpw hwspec hwf, ?_⟩
    rw [getLast?_cons_getD_eq]; exact hlast

lemma witness_reaches_sub {prev i : Seq} {o : List Msg}
    (h : WitnessReaches prev i o) : prev ⊑ i := by
  obtain ⟨w, hwf, hlast⟩ := h
  exact sub_trans (witness_from_sub_last hwf) hlast

lemma ghost_buster_witness_reaches
      (i prev : Seq) (o : List Msg)
      (ref : State -> Msg) (href : spec = ref) :
    WitnessReaches prev i o  ↔  GhostBusterRun i prev o ref ≠ {} := by
  induction prev, o using GhostBusterRun.induct with
  | case1 prev =>
    rw [← Set.nonempty_iff_ne_empty, GhostBusterRun]
    constructor
    · rintro ⟨w, hwf, hwprevi⟩
      cases hwf
      exact ⟨[], rfl, hwprevi⟩
    · rintro ⟨w, rfl, hprevi⟩
      exact ⟨[], .nil _, hprevi⟩
  | case2 prev o os ih =>
    rw [witness_reaches_cons, ← Set.nonempty_iff_ne_empty, GhostBusterRun]
    constructor
    · -- Completeness
      rintro ⟨w, hpw, hwspec, hvalid⟩
      obtain ⟨m, hmstep, hmw⟩ := min_sequence_exists prev w o hpw hwspec
      have hvalid' : WitnessReaches m i os := witness_reaches_mono hmw hvalid
      obtain ⟨ws, hws⟩ := Set.nonempty_iff_ne_empty.mpr ((ih m).mp hvalid')
      rw [href] at hmstep
      exact ⟨m :: ws, m, ⟨witness_reaches_sub hvalid', hmstep⟩, ws, hws, rfl⟩
    · -- soundness
      rintro ⟨x, w, ⟨hwi, hstep⟩, ws, hws, rfl⟩
      have hvalid : WitnessReaches w i os :=
        (ih w).mpr (Set.nonempty_iff_ne_empty.mp ⟨ws, hws⟩)
      rw [<-href] at hstep
      exact ⟨w, hstep.1, hstep.2.1, hvalid⟩

theorem ghost_buster_valid_from
        (i prev : Seq) (o : List Msg)
        (ref : State -> Msg) (href : spec = ref) :
    ValidFrom prev i o ↔ GhostBusterAlg i prev o ref ≠ {} := by
  rw [GhostBusterAlg, ← Set.nonempty_iff_ne_empty]
  constructor
  · rintro ⟨⟨w, hwf, hlast⟩, hispec⟩
    obtain ⟨x, hx⟩ := Set.nonempty_iff_ne_empty.mpr
      ((ghost_buster_witness_reaches i prev o ref href).mp ⟨w, hwf, hlast⟩)
    rw [href] at hispec
    exact ⟨x, hx, hispec⟩
  · rintro ⟨x, hx, hispec⟩
    obtain ⟨w, hwf, hlast⟩ :=
      (ghost_buster_witness_reaches i prev o ref href).mpr (Set.nonempty_iff_ne_empty.mp ⟨x, hx⟩)
    rw [<-href] at hispec
    exact ⟨⟨w, hwf, hlast⟩, hispec⟩

end GhostBuster
