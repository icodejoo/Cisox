exec(open('run_pb.py').read().split("J=[")[0])
SAR=['--prov','arena_extend_strategy=kSameAsRequested']; T='tok_main14ccm.json'
J=[('main14_base_p',X,T,[]),('main14_ext_p','_exp/main14-ext',T,[]),('main14_ext_beam2','_exp/main14-ext',T,['--beams','2']),
   ('main14_extg4_beam2','_exp/main14-g4-ext',T,['--beams','2']),('main14_share_beam2_p',X,T,['--share-emb','--beams','2'])]
exec("want="+open('run_pb.py').read().split("want=")[1])
